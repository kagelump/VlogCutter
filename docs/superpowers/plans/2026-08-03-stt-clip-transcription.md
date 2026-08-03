# STT Clip Transcription Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a local, offline, VAD-gated speech-to-text stage that transcribes a project's clips and stores the result as a derived artifact with recipe identity.

**Architecture:** A new `analysis` module inside `rcdvlog-core`. The risky arithmetic (window packing, timestamp translation, quality gating, coverage checking) is pure logic in small files with no I/O, tested exhaustively first. External surfaces — an FFmpeg child process for audio extraction and the `whisper-rs` crate for VAD + decode — sit behind two narrow traits so they are the only places that touch the outside world. A two-phase batch (cheap VAD sweep, then transcription with real weights) drives `batch-estimator`.

**Tech Stack:** Rust, `rusqlite`, `whisper-rs` 0.16 (vendors whisper.cpp + ggml, CPU-only), FFmpeg 7.x as a managed child process, `batch-estimator`, `clap` for CLI.

## Deviations taken during implementation

Recorded so the plan matches what was actually built.

1. **Module visibility.** The plan said `pub mod analysis;`. The crate's
   convention is a private `mod` plus a curated `pub use`, so `analysis` is
   private and only `STT_RECIPE_KEY`, `STT_TASK_KEY`, `TranscriptStatus` and
   `TranscribeOutcome` are re-exported.
2. **Migration entry point** is `migrate_project_catalog`, not the
   `apply_project_catalog_migrations` the plan guessed.
3. **`whisper-rs` API.** The plan's calls were written from the spike's prose and
   were wrong. The real 0.16 API is object-based: `state.get_segment(i)` returning
   a `WhisperSegment` with `start_timestamp`/`end_timestamp` (centiseconds),
   `to_str_lossy`, `n_tokens`, `get_token(i).token_probability()`, plus
   `full_lang_id_from_state()`. `new_with_params` takes `AsRef<Path>`. Auto-detect
   is `set_language(None)`.
4. **`SttEngine` takes samples, not a path.** As planned, `detect_speech` and
   `decode_window` both received a `&Path` and each re-read the file. On a corpus
   containing a 19-minute clip that re-reads and re-converts millions of samples
   per window. The trait now takes `&[f32]` and the pipeline reads once per clip.
5. **The two-phase batch was not built.** The plan called for a project-wide VAD
   sweep to produce exact 30-second window counts before weighting the batch.
   Implemented instead as one clip = one work unit. The sweep is not actually
   cheap end to end: it requires FFmpeg audio extraction for every clip up front,
   so it either doubles extraction work or requires retaining every clip's WAV.
   **Open:** clip-uniform weights skew the ETA, because ~64% of clips are silent
   and cost near nothing while a speech clip costs one or more full windows.
   Revisit with the measured window distribution.
6. **`transcript_coverage` added to the core.** The plan put a SQL query for
   `transcribe status` in the CLI; domain behavior belongs to the owning crate,
   so the query lives on `Catalog` and the CLI just serializes it.
7. **`#[allow(dead_code)]` on the analysis submodules** while the pipeline was
   assembled bottom-up. Remove once every unit has a consumer.

## Global Constraints

Copied verbatim from [the design](../specs/2026-08-03-stt-vlm-clip-analysis-design.md) and `AGENTS.md`. Every task's requirements implicitly include this section.

- Originals are immutable and referenced in place. Never move, rename, rewrite, or delete source media. **Audio is extracted from the proxy, never the original.**
- SQLite is canonical. No competing JSON store, no permanent Python service, no ONNX runtime.
- Domain behavior belongs to `rcdvlog-core`. CLI and desktop are adapters and must not reimplement it.
- Generated outputs live in project-local caches: `<project folder>/cache/...`. Never beside originals.
- CPU-only. Do not enable the `metal` feature of `whisper-rs` — it produces a binary that will not launch on macOS 14 (`_OBJC_CLASS_$_MTLResidencySetDescriptor` is macOS 15+).
- Recipe key, used verbatim as a string constant: `stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1`
- Batch estimator task key, used verbatim: `stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1-w1`
- Batch weight is **30-second windows**, not source bytes.
- Whisper window length is exactly **30000 ms**.
- Supported languages: English (`en`), Japanese (`ja`), Mandarin (`zh`). Cantonese is unsupported.
- Reject gate thresholds, used verbatim: `repetition_ratio >= 0.3` OR `mean_token_probability < 0.20`. Do **not** use `no_speech_probability()`; it was measured at median 0.000 for good and bad transcripts alike.
- Coverage shortfall threshold, used verbatim: re-split when `transcribed_ms < 0.65 * speech_ms` for a window.
- Model files, downloaded on demand into `<project folder>/cache/models/`: `ggml-large-v3-turbo-q5_0.bin` (574 MB) and `ggml-silero-v5.1.2.bin` (885 KB).
- CLI contract: one JSON success envelope on stdout, machine-readable JSON errors on stderr with nonzero exit, JSON progress events on stderr for long batches, no interactive prompts.
- Run `cargo fmt` and `cargo clippy --all-targets -- -D warnings` before every commit.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `core/rcdvlog-core/src/analysis/mod.rs` | Module exports, recipe key constants |
| `core/rcdvlog-core/src/analysis/windows.rs` | Speech regions, 30 s window packing, timestamp translation back to clip time |
| `core/rcdvlog-core/src/analysis/quality.rs` | Repetition ratio, mean token probability, reject gate |
| `core/rcdvlog-core/src/analysis/coverage.rs` | Coverage shortfall detection and re-split decision |
| `core/rcdvlog-core/src/analysis/audio.rs` | `AudioExtractor` trait + FFmpeg implementation (16 kHz mono WAV from proxy) |
| `core/rcdvlog-core/src/analysis/engine.rs` | `SttEngine` trait, request/response types, model download |
| `core/rcdvlog-core/src/analysis/whisper_engine.rs` | `whisper-rs` implementation of `SttEngine` (the only file that names whisper-rs types) |
| `core/rcdvlog-core/src/analysis/store.rs` | Transcript persistence, staleness by recipe key + source fingerprint |
| `core/rcdvlog-core/src/analysis/stt.rs` | Pipeline orchestration, two-phase batch, estimator wiring |
| `core/rcdvlog-core/src/migrations.rs` | Migration 13 (modify) |
| `apps/cli/src/main.rs` | `transcribe` subcommands (modify) |

Files 2–5 are pure logic with no I/O and no external dependency. They are built and tested first because they carry the failure modes the spike measured.

---

### Task 1: Migration 13 — transcript tables

**Files:**
- Modify: `core/rcdvlog-core/src/migrations.rs` (bump `PROJECT_CATALOG_SCHEMA_VERSION`, extend `PROJECT_CATALOG_MIGRATIONS`, add `add_transcripts`)

**Interfaces:**
- Consumes: nothing.
- Produces: tables `asset_transcripts` and `transcript_segments`; `PROJECT_CATALOG_SCHEMA_VERSION == 13`.

- [ ] **Step 1: Write the failing test**

Add to the test module at the bottom of `core/rcdvlog-core/src/migrations.rs`:

```rust
#[test]
fn migration_13_creates_transcript_tables() {
    let mut connection = Connection::open_in_memory().unwrap();
    apply_project_catalog_migrations(&mut connection).unwrap();

    let transcript_columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('asset_transcripts')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    assert!(transcript_columns.contains(&"recipe_key".to_string()));
    assert!(transcript_columns.contains(&"source_fingerprint".to_string()));
    assert!(transcript_columns.contains(&"status".to_string()));

    let segment_columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('transcript_segments')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    assert!(segment_columns.contains(&"start_ms".to_string()));
    assert!(segment_columns.contains(&"mean_token_probability".to_string()));

    assert_eq!(PROJECT_CATALOG_SCHEMA_VERSION, 13);
}

#[test]
fn transcript_status_rejects_unknown_value() {
    let mut connection = Connection::open_in_memory().unwrap();
    apply_project_catalog_migrations(&mut connection).unwrap();
    connection
        .execute(
            "INSERT INTO projects (id, name, created_at) VALUES ('p1', 'p', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes,
             duration_seconds, created_at)
             VALUES ('a1', 'p1', 'f1', '/x.mp4', 'x.mp4', 1, 1.0, '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    let bad = connection.execute(
        "INSERT INTO asset_transcripts (asset_id, recipe_key, source_kind, source_fingerprint,
         status, created_at)
         VALUES ('a1', 'r', 'proxy', 'pf', 'banana', '2026-01-01T00:00:00Z')",
        [],
    );
    assert!(bad.is_err(), "CHECK constraint should reject unknown status");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core migration_13_creates_transcript_tables -- --nocapture`
Expected: FAIL — `no such table: asset_transcripts`.

- [ ] **Step 3: Write minimal implementation**

Change the constant near the top of the file:

```rust
pub const PROJECT_CATALOG_SCHEMA_VERSION: u32 = 13;
```

Append to `PROJECT_CATALOG_MIGRATIONS`, after the `id: 12` entry:

```rust
    SqliteMigration {
        id: 13,
        name: "transcripts",
        source_identity: "project-catalog/0013-transcripts/20260803",
        apply: add_transcripts,
    },
```

Add the function next to `add_edit_plan`:

```rust
/// Migration 13. Local speech-to-text results as derived artifacts.
///
/// `asset_transcripts` is one row per asset because a transcript is identified
/// by its recipe key plus the fingerprint of the proxy it was derived from; a
/// second recipe supersedes the first rather than coexisting with it.
///
/// `status` distinguishes the three non-failure outcomes the pipeline can
/// produce. `no_speech` is a successful, meaningful result — the VAD gate found
/// nothing and no model was ever invoked — and must not be confused with
/// `failed`.
fn add_transcripts(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS asset_transcripts (
            asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
            recipe_key TEXT NOT NULL,
            source_kind TEXT NOT NULL DEFAULT 'proxy',
            source_fingerprint TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('speech', 'no_speech', 'rejected', 'failed')),
            reject_reason TEXT NOT NULL DEFAULT '',
            primary_language TEXT NOT NULL DEFAULT '',
            speech_seconds REAL NOT NULL DEFAULT 0,
            window_count INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS transcript_segments (
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            segment_index INTEGER NOT NULL,
            start_ms INTEGER NOT NULL,
            end_ms INTEGER NOT NULL,
            language TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL,
            mean_token_probability REAL NOT NULL DEFAULT 0,
            PRIMARY KEY(asset_id, segment_index)
        );

        CREATE INDEX IF NOT EXISTS transcript_segments_time
        ON transcript_segments(asset_id, start_ms);
        "#,
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core migration_13 && cargo test -p rcdvlog-core transcript_status_rejects`
Expected: PASS. Then run the whole crate to confirm no existing migration test hardcodes version 12:
Run: `cargo test -p rcdvlog-core`
Expected: PASS. If a test asserts `PROJECT_CATALOG_SCHEMA_VERSION == 12`, update it to 13 — that is the intended change, not a regression.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/src/migrations.rs
git commit -m "feat(analysis): migration 13 adds transcript tables"
```

---

### Task 2: Speech regions and 30-second window packing

**Files:**
- Create: `core/rcdvlog-core/src/analysis/mod.rs`
- Create: `core/rcdvlog-core/src/analysis/windows.rs`
- Modify: `core/rcdvlog-core/src/lib.rs` (add `pub mod analysis;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub const STT_RECIPE_KEY: &str`
  - `pub const WINDOW_MS: u32 = 30_000`
  - `pub struct SpeechRegion { pub start_ms: u32, pub end_ms: u32 }` with `pub fn duration_ms(&self) -> u32`
  - `pub struct PackedRegion { pub window_offset_ms: u32, pub source_start_ms: u32, pub duration_ms: u32 }`
  - `pub struct PackedWindow { pub regions: Vec<PackedRegion> }` with `pub fn speech_ms(&self) -> u32`
  - `pub fn pack_windows(regions: &[SpeechRegion]) -> Vec<PackedWindow>`

**Why this exists:** round 1 Finding 3 measured that whisper bills a full 30 s window per decode regardless of how much audio is in it (decode time correlated with chunk length at only r = 0.29). The PRD's 8–25 s chunking was 30% slower while processing 39% less audio. Packing speech into as few full windows as possible is the fix.

- [ ] **Step 1: Write the failing test**

Create `core/rcdvlog-core/src/analysis/windows.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn region(start_ms: u32, end_ms: u32) -> SpeechRegion {
        SpeechRegion { start_ms, end_ms }
    }

    #[test]
    fn no_regions_produces_no_windows() {
        assert!(pack_windows(&[]).is_empty());
    }

    #[test]
    fn short_regions_share_one_window() {
        let windows = pack_windows(&[region(1_000, 4_000), region(9_000, 12_000)]);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].regions.len(), 2);
        assert_eq!(windows[0].regions[0].window_offset_ms, 0);
        assert_eq!(windows[0].regions[0].source_start_ms, 1_000);
        assert_eq!(windows[0].regions[0].duration_ms, 3_000);
        // Silence between the regions is dropped, so the second region starts
        // immediately after the first inside the window.
        assert_eq!(windows[0].regions[1].window_offset_ms, 3_000);
        assert_eq!(windows[0].regions[1].source_start_ms, 9_000);
        assert_eq!(windows[0].speech_ms(), 6_000);
    }

    #[test]
    fn a_region_that_does_not_fit_starts_a_new_window_rather_than_being_cut() {
        // 20 s then 15 s: the second cannot fit in the remaining 10 s.
        let windows = pack_windows(&[region(0, 20_000), region(30_000, 45_000)]);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].regions.len(), 1);
        assert_eq!(windows[1].regions.len(), 1);
        assert_eq!(windows[1].regions[0].window_offset_ms, 0);
        assert_eq!(windows[1].regions[0].source_start_ms, 30_000);
        assert_eq!(windows[1].regions[0].duration_ms, 15_000);
    }

    #[test]
    fn a_region_longer_than_one_window_is_split_across_windows() {
        let windows = pack_windows(&[region(0, 70_000)]);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].regions[0].duration_ms, 30_000);
        assert_eq!(windows[0].regions[0].source_start_ms, 0);
        assert_eq!(windows[1].regions[0].duration_ms, 30_000);
        assert_eq!(windows[1].regions[0].source_start_ms, 30_000);
        assert_eq!(windows[2].regions[0].duration_ms, 10_000);
        assert_eq!(windows[2].regions[0].source_start_ms, 60_000);
    }

    #[test]
    fn no_window_ever_exceeds_thirty_seconds() {
        let regions: Vec<SpeechRegion> = (0..40)
            .map(|i| region(i * 5_000, i * 5_000 + 4_000))
            .collect();
        for window in pack_windows(&regions) {
            assert!(window.speech_ms() <= WINDOW_MS, "window overflowed");
        }
    }

    #[test]
    fn zero_length_regions_are_dropped() {
        let windows = pack_windows(&[region(1_000, 1_000), region(2_000, 5_000)]);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].regions.len(), 1);
        assert_eq!(windows[0].regions[0].source_start_ms, 2_000);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core analysis::windows`
Expected: FAIL to compile — `analysis` module does not exist.

- [ ] **Step 3: Write minimal implementation**

Create `core/rcdvlog-core/src/analysis/mod.rs`:

```rust
//! Derived per-clip analysis: local speech-to-text today, cloud vision later.
//!
//! Results are derived artifacts with recipe identity, in the same style as
//! proxies and thumbnails: re-running an unchanged recipe against an unchanged
//! source is idempotent, and a changed recipe makes stored results detectably
//! stale rather than silently mixed.

pub mod windows;

/// Identity of the speech-to-text recipe. Changing any component of the
/// pipeline — model, quantization, VAD model, or window strategy — requires a
/// new key so existing transcripts become detectably stale.
pub const STT_RECIPE_KEY: &str = "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1";

/// Batch-estimator task key. Suffixed with the worker count because throughput
/// is not comparable across concurrency levels.
pub const STT_TASK_KEY: &str = "stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1-w1";
```

Prepend to `core/rcdvlog-core/src/analysis/windows.rs`, above the test module:

```rust
//! Packing detected speech into whisper's fixed 30-second decode windows.
//!
//! whisper.cpp bills a full 30-second window per decode regardless of how much
//! audio the window actually contains, so the cost of a clip is its window
//! count, not its duration. Packing non-adjacent speech regions into as few
//! windows as possible is therefore the dominant cost lever after the VAD gate
//! itself.
//!
//! Every packed region records where it came from, because the timestamps
//! whisper returns are relative to the packed window and are meaningless until
//! they are translated back to clip time.

/// whisper's fixed decode window.
pub const WINDOW_MS: u32 = 30_000;

/// A span of detected speech in clip time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeechRegion {
    pub start_ms: u32,
    pub end_ms: u32,
}

impl SpeechRegion {
    pub fn duration_ms(&self) -> u32 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// A speech region placed inside a packed window, retaining its origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedRegion {
    /// Offset of this region from the start of the packed window.
    pub window_offset_ms: u32,
    /// Where this audio came from in clip time.
    pub source_start_ms: u32,
    pub duration_ms: u32,
}

/// One 30-second decode unit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackedWindow {
    pub regions: Vec<PackedRegion>,
}

impl PackedWindow {
    /// Total speech packed into this window. Never exceeds `WINDOW_MS`.
    pub fn speech_ms(&self) -> u32 {
        self.regions.iter().map(|region| region.duration_ms).sum()
    }
}

/// Packs speech regions into as few 30-second windows as possible.
///
/// A region that does not fit in the current window starts a new one rather
/// than being cut mid-utterance. A region longer than a whole window is the one
/// case where splitting is unavoidable, and it is split on window boundaries.
pub fn pack_windows(regions: &[SpeechRegion]) -> Vec<PackedWindow> {
    let mut windows: Vec<PackedWindow> = Vec::new();
    let mut current = PackedWindow::default();
    let mut used_ms = 0_u32;

    for region in regions {
        let mut source_start = region.start_ms;
        let mut remaining = region.duration_ms();
        if remaining == 0 {
            continue;
        }

        while remaining > 0 {
            let free = WINDOW_MS - used_ms;
            // Start a new window when this region cannot fit whole, unless the
            // current window is empty — in which case the region is longer than
            // a window and must be split.
            if remaining > free && used_ms > 0 {
                windows.push(std::mem::take(&mut current));
                used_ms = 0;
                continue;
            }
            let take = remaining.min(WINDOW_MS - used_ms);
            current.regions.push(PackedRegion {
                window_offset_ms: used_ms,
                source_start_ms: source_start,
                duration_ms: take,
            });
            used_ms += take;
            source_start += take;
            remaining -= take;
            if used_ms == WINDOW_MS {
                windows.push(std::mem::take(&mut current));
                used_ms = 0;
            }
        }
    }

    if !current.regions.is_empty() {
        windows.push(current);
    }
    windows
}
```

Add to `core/rcdvlog-core/src/lib.rs`, alongside the other `pub mod` declarations near the top:

```rust
pub mod analysis;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core analysis::windows`
Expected: PASS, 6 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/src/analysis core/rcdvlog-core/src/lib.rs
git commit -m "feat(analysis): pack speech regions into 30s whisper windows"
```

---

### Task 3: Translating window timestamps back to clip time

**Files:**
- Modify: `core/rcdvlog-core/src/analysis/windows.rs`

**Interfaces:**
- Consumes: `PackedWindow`, `PackedRegion` from Task 2.
- Produces: `pub fn window_offset_to_source_ms(window: &PackedWindow, offset_ms: u32) -> Option<u32>` and `pub fn translate_span(window: &PackedWindow, start_offset_ms: u32, end_offset_ms: u32) -> Option<(u32, u32)>`.

**Why this exists:** the design names this "the highest-risk piece of arithmetic in the stage". After concatenating non-adjacent speech regions, every timestamp whisper returns points into the packed window, not the clip. A wrong translation silently misplaces every transcript segment, and nothing downstream would notice.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `core/rcdvlog-core/src/analysis/windows.rs`:

```rust
    #[test]
    fn offset_inside_first_region_maps_to_its_source_time() {
        let windows = pack_windows(&[region(1_000, 4_000), region(9_000, 12_000)]);
        let window = &windows[0];
        assert_eq!(window_offset_to_source_ms(window, 0), Some(1_000));
        assert_eq!(window_offset_to_source_ms(window, 2_500), Some(3_500));
    }

    #[test]
    fn offset_inside_a_later_region_skips_the_dropped_silence() {
        let windows = pack_windows(&[region(1_000, 4_000), region(9_000, 12_000)]);
        let window = &windows[0];
        // Window offset 3000 is the boundary: the first region ended, so this is
        // the start of the second region at clip time 9000, not 4000.
        assert_eq!(window_offset_to_source_ms(window, 3_000), Some(9_000));
        assert_eq!(window_offset_to_source_ms(window, 4_000), Some(10_000));
    }

    #[test]
    fn offset_past_the_packed_content_is_clamped_to_the_end() {
        // whisper pads to a full 30s window, so it can return timestamps beyond
        // the audio that was actually packed.
        let windows = pack_windows(&[region(1_000, 4_000)]);
        let window = &windows[0];
        assert_eq!(window_offset_to_source_ms(window, 25_000), Some(4_000));
    }

    #[test]
    fn empty_window_translates_nothing() {
        let window = PackedWindow::default();
        assert_eq!(window_offset_to_source_ms(&window, 0), None);
    }

    #[test]
    fn a_span_crossing_a_region_boundary_uses_each_ends_own_region() {
        let windows = pack_windows(&[region(1_000, 4_000), region(9_000, 12_000)]);
        let window = &windows[0];
        // 2000..4000 in window time spans the boundary at 3000.
        assert_eq!(translate_span(window, 2_000, 4_000), Some((3_000, 10_000)));
    }

    #[test]
    fn a_span_never_ends_before_it_starts() {
        let windows = pack_windows(&[region(5_000, 8_000)]);
        let window = &windows[0];
        assert_eq!(translate_span(window, 2_000, 1_000), Some((7_000, 7_000)));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core analysis::windows`
Expected: FAIL to compile — `window_offset_to_source_ms` not found.

- [ ] **Step 3: Write minimal implementation**

Append to `core/rcdvlog-core/src/analysis/windows.rs`, above the test module:

```rust
/// Translates an offset in packed-window time back to clip time.
///
/// Returns `None` only for an empty window. An offset past the end of the
/// packed audio is clamped to the end of the last region, because whisper pads
/// short windows to a full 30 seconds and will happily return timestamps inside
/// that padding.
pub fn window_offset_to_source_ms(window: &PackedWindow, offset_ms: u32) -> Option<u32> {
    let last = window.regions.last()?;
    for region in &window.regions {
        let region_end = region.window_offset_ms + region.duration_ms;
        // The half-open interval means an offset exactly on a boundary belongs
        // to the region that starts there, not the one that ends there.
        if offset_ms >= region.window_offset_ms && offset_ms < region_end {
            return Some(region.source_start_ms + (offset_ms - region.window_offset_ms));
        }
    }
    Some(last.source_start_ms + last.duration_ms)
}

/// Translates a window-time span to a clip-time span.
///
/// Each end is translated through its own region, so a span crossing dropped
/// silence widens to cover the real elapsed clip time. The result is ordered.
pub fn translate_span(
    window: &PackedWindow,
    start_offset_ms: u32,
    end_offset_ms: u32,
) -> Option<(u32, u32)> {
    let start = window_offset_to_source_ms(window, start_offset_ms)?;
    let end = window_offset_to_source_ms(window, end_offset_ms)?;
    Some((start.min(end), start.max(end)))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core analysis::windows`
Expected: PASS, 12 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/src/analysis/windows.rs
git commit -m "feat(analysis): translate packed-window timestamps to clip time"
```

---

### Task 4: Repetition ratio and the reject gate

**Files:**
- Create: `core/rcdvlog-core/src/analysis/quality.rs`
- Modify: `core/rcdvlog-core/src/analysis/mod.rs` (add `pub mod quality;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct DecodedSegment { pub start_ms: u32, pub end_ms: u32, pub language: String, pub text: String, pub mean_token_probability: f32 }`
  - `pub fn repetition_ratio(segments: &[DecodedSegment]) -> f32`
  - `pub fn mean_token_probability(segments: &[DecodedSegment]) -> f32`
  - `pub enum RejectReason { Repetition, LowTokenProbability }` with `pub fn as_str(&self) -> &'static str`
  - `pub fn reject_reason(segments: &[DecodedSegment]) -> Option<RejectReason>`

**Why this exists:** round 1 Finding 12 measured that `no_speech_probability()` has *zero* separating power (median 0.000 for good and bad alike) and that language confidence is a poor rejection gate (rejects 15/29 good chunks). Repetition ratio was the strong signal — median 0.929 on bad chunks versus 0.000 on good — and the combined gate caught 4/4 bad with 0/29 false rejections. The `>= 0.3` and `< 0.20` constants rest on only 4 positive examples and are configuration, not truth.

- [ ] **Step 1: Write the failing test**

Create `core/rcdvlog-core/src/analysis/quality.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn segment(text: &str, probability: f32) -> DecodedSegment {
        DecodedSegment {
            start_ms: 0,
            end_ms: 1_000,
            language: "ja".to_string(),
            text: text.to_string(),
            mean_token_probability: probability,
        }
    }

    #[test]
    fn distinct_segments_have_no_repetition() {
        let segments = vec![segment("次は新大阪です", 0.8), segment("ありがとう", 0.8)];
        assert_eq!(repetition_ratio(&segments), 0.0);
    }

    #[test]
    fn the_measured_hallucination_shape_scores_high() {
        // Round 1 Finding 2: whisper returned the same sentence six times on a
        // low-speech clip.
        let segments: Vec<DecodedSegment> =
            (0..6).map(|_| segment("ご飯を食べてみます。", 0.7)).collect();
        // Five of six segments duplicate an earlier one.
        assert!((repetition_ratio(&segments) - 5.0 / 6.0).abs() < 1e-6);
    }

    #[test]
    fn repetition_ignores_case_whitespace_and_punctuation() {
        let segments = vec![
            segment("Go straight and turn left.", 0.8),
            segment("  go straight and turn left  ", 0.8),
        ];
        assert!((repetition_ratio(&segments) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn empty_input_is_not_repetitive_and_is_not_rejected() {
        assert_eq!(repetition_ratio(&[]), 0.0);
        assert_eq!(mean_token_probability(&[]), 0.0);
        assert!(reject_reason(&[]).is_none());
    }

    #[test]
    fn mean_token_probability_averages_across_segments() {
        let segments = vec![segment("a", 0.4), segment("b", 0.6)];
        assert!((mean_token_probability(&segments) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_repetitive_transcript_is_rejected() {
        let segments: Vec<DecodedSegment> =
            (0..6).map(|_| segment("ご飯を食べてみます。", 0.7)).collect();
        assert_eq!(reject_reason(&segments), Some(RejectReason::Repetition));
    }

    #[test]
    fn a_low_confidence_transcript_is_rejected() {
        // Round 1 Finding 12: the fourth bad chunk scored 0.11 against a
        // good-chunk minimum of 0.207.
        let segments = vec![segment("garbled", 0.11), segment("nonsense", 0.11)];
        assert_eq!(
            reject_reason(&segments),
            Some(RejectReason::LowTokenProbability)
        );
    }

    #[test]
    fn a_good_transcript_is_not_rejected() {
        let segments = vec![
            segment("お客様、当日券購入希望のお客様は", 0.73),
            segment("正面のチケット引き換え所にお進みください。", 0.73),
        ];
        assert!(reject_reason(&segments).is_none());
    }

    #[test]
    fn a_single_segment_is_never_repetitive() {
        let segments = vec![segment("hello", 0.8)];
        assert_eq!(repetition_ratio(&segments), 0.0);
        assert!(reject_reason(&segments).is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core analysis::quality`
Expected: FAIL to compile — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add to `core/rcdvlog-core/src/analysis/mod.rs`:

```rust
pub mod quality;
```

Prepend to `core/rcdvlog-core/src/analysis/quality.rs`, above the test module:

```rust
//! The hallucination gate for local transcription.
//!
//! Round 1 measured that the signals whisper hands out for free mostly do not
//! work. `no_speech_probability()` had median 0.000 on good and bad transcripts
//! alike — zero separating power — and is deliberately not used here. Language
//! confidence predicts quality but rejects 15 of 29 good chunks when used as a
//! gate. Repetition is the signal that separates: median 0.929 on bad chunks
//! against 0.000 on good ones.
//!
//! The thresholds below were validated against only four positive examples.
//! They are starting constants to revisit against a larger hand-labeled set,
//! not measured truth.

use std::collections::HashSet;

/// Reject a transcript once this fraction of its segments duplicate an earlier
/// segment. Bad chunks measured a median of 0.929; good chunks measured 0.000.
const REPETITION_REJECT_RATIO: f32 = 0.3;

/// Reject a transcript below this mean token probability. The one bad chunk
/// repetition did not catch scored 0.11; the lowest good chunk scored 0.207.
const MIN_MEAN_TOKEN_PROBABILITY: f32 = 0.20;

/// One decoded segment in clip time.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedSegment {
    pub start_ms: u32,
    pub end_ms: u32,
    pub language: String,
    pub text: String,
    pub mean_token_probability: f32,
}

/// Why a transcript was rejected. Recorded on the stored row so a rejection is
/// explainable rather than an unexplained absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    Repetition,
    LowTokenProbability,
}

impl RejectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            RejectReason::Repetition => "repetition",
            RejectReason::LowTokenProbability => "low_token_probability",
        }
    }
}

/// Collapses a segment to its comparable form.
///
/// Case, surrounding whitespace and trailing punctuation vary between otherwise
/// identical hallucinated repeats, so comparing raw text would miss them.
fn normalize(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
        .flat_map(|character| character.to_lowercase())
        .collect()
}

/// Fraction of segments that duplicate an earlier segment's normalized text.
///
/// Six identical segments score 5/6; all-distinct segments score 0.
pub fn repetition_ratio(segments: &[DecodedSegment]) -> f32 {
    if segments.len() < 2 {
        return 0.0;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut duplicates = 0_usize;
    for segment in segments {
        let normalized = normalize(&segment.text);
        if normalized.is_empty() {
            continue;
        }
        if !seen.insert(normalized) {
            duplicates += 1;
        }
    }
    duplicates as f32 / segments.len() as f32
}

/// Mean of the per-segment mean token probabilities.
pub fn mean_token_probability(segments: &[DecodedSegment]) -> f32 {
    if segments.is_empty() {
        return 0.0;
    }
    let total: f32 = segments
        .iter()
        .map(|segment| segment.mean_token_probability)
        .sum();
    total / segments.len() as f32
}

/// The measured gate: reject on repetition, or on very low token probability.
///
/// An empty transcript is not a rejection — that is the VAD gate's business,
/// and a clip with no speech is a successful `no_speech` result.
pub fn reject_reason(segments: &[DecodedSegment]) -> Option<RejectReason> {
    if segments.is_empty() {
        return None;
    }
    if repetition_ratio(segments) >= REPETITION_REJECT_RATIO {
        return Some(RejectReason::Repetition);
    }
    if mean_token_probability(segments) < MIN_MEAN_TOKEN_PROBABILITY {
        return Some(RejectReason::LowTokenProbability);
    }
    None
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core analysis::quality`
Expected: PASS, 9 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/src/analysis
git commit -m "feat(analysis): reject gate on repetition and token probability"
```

---

### Task 5: Coverage shortfall detection and re-split

**Files:**
- Create: `core/rcdvlog-core/src/analysis/coverage.rs`
- Modify: `core/rcdvlog-core/src/analysis/mod.rs` (add `pub mod coverage;`)

**Interfaces:**
- Consumes: `PackedWindow` (Task 2), `DecodedSegment` (Task 4).
- Produces:
  - `pub fn transcribed_ms(segments: &[DecodedSegment]) -> u32`
  - `pub fn needs_resplit(window: &PackedWindow, segments: &[DecodedSegment]) -> bool`
  - `pub fn resplit(window: &PackedWindow) -> Vec<PackedWindow>`

**Why this exists:** round 1 Finding 9 fed seven code-switched clips through as single windows. In **three of seven**, an entire language's speech vanished from the transcript with no error and no low-confidence signal. Packing (Task 2) makes this *more* likely, because a fuller window is more likely to span a language switch. The resolution is not to stop packing but to detect the shortfall — the failures dropped whole utterances, so the gap is large — and re-split that window at its internal silence boundaries, which are exactly the likely switch points.

- [ ] **Step 1: Write the failing test**

Create `core/rcdvlog-core/src/analysis/coverage.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::windows::{SpeechRegion, pack_windows};

    fn segment(start_ms: u32, end_ms: u32) -> DecodedSegment {
        DecodedSegment {
            start_ms,
            end_ms,
            language: "ja".to_string(),
            text: "text".to_string(),
            mean_token_probability: 0.7,
        }
    }

    fn two_region_window() -> PackedWindow {
        // 6s of Japanese then 6s of English, the Finding 9 shape.
        pack_windows(&[
            SpeechRegion { start_ms: 0, end_ms: 6_000 },
            SpeechRegion { start_ms: 10_000, end_ms: 16_000 },
        ])
        .remove(0)
    }

    #[test]
    fn transcribed_duration_sums_segments() {
        let segments = vec![segment(0, 3_000), segment(4_000, 6_000)];
        assert_eq!(transcribed_ms(&segments), 5_000);
    }

    #[test]
    fn overlapping_segments_are_not_double_counted() {
        let segments = vec![segment(0, 4_000), segment(3_000, 6_000)];
        assert_eq!(transcribed_ms(&segments), 6_000);
    }

    #[test]
    fn unordered_segments_are_merged_correctly() {
        let segments = vec![segment(8_000, 10_000), segment(0, 2_000)];
        assert_eq!(transcribed_ms(&segments), 4_000);
    }

    #[test]
    fn full_coverage_does_not_resplit() {
        let window = two_region_window();
        // 12s packed, 11.5s transcribed.
        let segments = vec![segment(0, 5_500), segment(6_000, 12_000)];
        assert!(!needs_resplit(&window, &segments));
    }

    #[test]
    fn a_dropped_language_half_triggers_resplit() {
        let window = two_region_window();
        // Only the Japanese half came back: 6s of 12s.
        let segments = vec![segment(0, 6_000)];
        assert!(needs_resplit(&window, &segments));
    }

    #[test]
    fn a_single_region_window_never_resplits() {
        // There is no internal silence boundary to split on, so re-splitting
        // would just repeat the same decode.
        let window = pack_windows(&[SpeechRegion { start_ms: 0, end_ms: 20_000 }]).remove(0);
        assert!(!needs_resplit(&window, &[]));
    }

    #[test]
    fn an_empty_window_never_resplits() {
        assert!(!needs_resplit(&PackedWindow::default(), &[]));
    }

    #[test]
    fn resplit_produces_one_window_per_region_rebased_to_zero() {
        let window = two_region_window();
        let split = resplit(&window);
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].regions.len(), 1);
        assert_eq!(split[0].regions[0].window_offset_ms, 0);
        assert_eq!(split[0].regions[0].source_start_ms, 0);
        assert_eq!(split[1].regions[0].window_offset_ms, 0);
        assert_eq!(split[1].regions[0].source_start_ms, 10_000);
        assert_eq!(split[1].regions[0].duration_ms, 6_000);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core analysis::coverage`
Expected: FAIL to compile — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add to `core/rcdvlog-core/src/analysis/mod.rs`:

```rust
pub mod coverage;
```

Prepend to `core/rcdvlog-core/src/analysis/coverage.rs`, above the test module:

```rust
//! Detecting the silent language-drop that packing makes more likely.
//!
//! Round 1 fed seven code-switched clips through as single windows. In three of
//! the seven, an entire language's speech vanished from the transcript with no
//! error and no low-confidence signal — the `en_ja`, `ja_en_ja` and `en_es`
//! cases. Because a whole utterance disappears, the shortfall against the audio
//! that went into the window is large and easy to detect.
//!
//! The response is to re-decode that window one speech region at a time. The
//! silence boundaries between regions are exactly where a speaker is likely to
//! have switched language, so splitting there targets the failure directly. Only
//! the minority of windows that actually trip the check pay the extra decode.

use crate::analysis::quality::DecodedSegment;
use crate::analysis::windows::{PackedWindow, WINDOW_MS};

/// Re-split when a window returns less than this fraction of the speech that
/// went into it. Finding 9's failures dropped roughly half a window's content,
/// so this sits comfortably below a healthy decode and above a dropped half.
const MIN_COVERAGE_RATIO: f32 = 0.65;

/// Total distinct time covered by the returned segments.
///
/// Segments are merged before summing, because whisper can return overlapping
/// segments and double-counting would mask a real shortfall.
pub fn transcribed_ms(segments: &[DecodedSegment]) -> u32 {
    if segments.is_empty() {
        return 0;
    }
    let mut spans: Vec<(u32, u32)> = segments
        .iter()
        .map(|segment| {
            (
                segment.start_ms.min(segment.end_ms),
                segment.start_ms.max(segment.end_ms),
            )
        })
        .collect();
    spans.sort_unstable();

    let mut total = 0_u32;
    let (mut current_start, mut current_end) = spans[0];
    for (start, end) in spans.into_iter().skip(1) {
        if start <= current_end {
            current_end = current_end.max(end);
        } else {
            total += current_end - current_start;
            current_start = start;
            current_end = end;
        }
    }
    total + (current_end - current_start)
}

/// Whether this window's decode looks like it dropped a language.
///
/// A window with fewer than two regions has no internal silence boundary to
/// split on, so re-splitting could only repeat the identical decode.
pub fn needs_resplit(window: &PackedWindow, segments: &[DecodedSegment]) -> bool {
    if window.regions.len() < 2 {
        return false;
    }
    let speech_ms = window.speech_ms();
    if speech_ms == 0 {
        return false;
    }
    let covered = transcribed_ms(segments).min(WINDOW_MS);
    (covered as f32) < MIN_COVERAGE_RATIO * speech_ms as f32
}

/// Splits a window into one window per speech region.
///
/// Each region is rebased to offset zero because it becomes its own decode unit.
pub fn resplit(window: &PackedWindow) -> Vec<PackedWindow> {
    window
        .regions
        .iter()
        .map(|region| PackedWindow {
            regions: vec![crate::analysis::windows::PackedRegion {
                window_offset_ms: 0,
                source_start_ms: region.source_start_ms,
                duration_ms: region.duration_ms,
            }],
        })
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core analysis::coverage`
Expected: PASS, 8 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/src/analysis
git commit -m "feat(analysis): detect dropped-language shortfall and re-split"
```

---

### Task 6: Audio extraction from the proxy

**Files:**
- Create: `core/rcdvlog-core/src/analysis/audio.rs`
- Modify: `core/rcdvlog-core/src/analysis/mod.rs` (add `pub mod audio;`)

**Interfaces:**
- Consumes: `tool_command` from `crate::lib` (already used by the proxy path; make it `pub(crate)` if it is private).
- Produces:
  - `pub trait AudioExtractor { fn extract_pcm16k(&self, source: &Path, destination: &Path) -> Result<()>; }`
  - `pub struct FfmpegAudioExtractor;` implementing it
  - `pub fn audio_extract_command(source: &Path, destination: &Path) -> Command`

**Why this exists:** whisper needs 16 kHz mono PCM. Audio comes from the **proxy**, never the original — the original may be offline on the NAS, and the design requires the stage to work when only proxies are present. The trait exists so the pipeline in Task 9 can be tested without FFmpeg.

- [ ] **Step 1: Write the failing test**

Create `core/rcdvlog-core/src/analysis/audio.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn rendered_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn extraction_requests_16khz_mono_signed_16bit_pcm() {
        let command = audio_extract_command(Path::new("/cache/p.mp4"), Path::new("/tmp/o.wav"));
        let args = rendered_args(&command);
        assert!(args.windows(2).any(|pair| pair == ["-ar", "16000"]));
        assert!(args.windows(2).any(|pair| pair == ["-ac", "1"]));
        assert!(args.windows(2).any(|pair| pair == ["-acodec", "pcm_s16le"]));
    }

    #[test]
    fn extraction_drops_video_and_overwrites() {
        let command = audio_extract_command(Path::new("/cache/p.mp4"), Path::new("/tmp/o.wav"));
        let args = rendered_args(&command);
        assert!(args.iter().any(|arg| arg == "-vn"));
        assert!(args.iter().any(|arg| arg == "-y"));
    }

    #[test]
    fn extraction_reads_the_given_source_and_writes_the_given_destination() {
        let command = audio_extract_command(Path::new("/cache/p.mp4"), Path::new("/tmp/o.wav"));
        let args = rendered_args(&command);
        let input_index = args.iter().position(|arg| arg == "-i").unwrap();
        assert_eq!(args[input_index + 1], "/cache/p.mp4");
        assert_eq!(args.last().unwrap(), "/tmp/o.wav");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core analysis::audio`
Expected: FAIL to compile — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add to `core/rcdvlog-core/src/analysis/mod.rs`:

```rust
pub mod audio;
```

Prepend to `core/rcdvlog-core/src/analysis/audio.rs`:

```rust
//! Extracting whisper-ready audio from a clip's review proxy.
//!
//! Audio always comes from the proxy, never the original. The original lives on
//! a NAS that may be offline, and the design requires transcription to work
//! whenever proxies are present. FFmpeg runs as a managed child process, in the
//! same style as proxy and thumbnail generation.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

use crate::tool_command;

/// Extracts 16 kHz mono PCM, the only input format whisper.cpp accepts.
pub trait AudioExtractor {
    fn extract_pcm16k(&self, source: &Path, destination: &Path) -> Result<()>;
}

/// Builds the extraction command. Separated from execution so the argument
/// contract can be asserted without invoking FFmpeg.
pub fn audio_extract_command(source: &Path, destination: &Path) -> Command {
    let mut command = tool_command("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(source)
        .arg("-vn")
        .arg("-ar")
        .arg("16000")
        .arg("-ac")
        .arg("1")
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg(destination);
    command
}

pub struct FfmpegAudioExtractor;

impl AudioExtractor for FfmpegAudioExtractor {
    fn extract_pcm16k(&self, source: &Path, destination: &Path) -> Result<()> {
        if !source.is_file() {
            bail!("proxy is offline: {}", source.display());
        }
        let status = audio_extract_command(source, destination)
            .status()
            .context("spawn ffmpeg audio extraction process")?;
        if !status.success() {
            bail!("ffmpeg audio extraction failed with {status}");
        }
        Ok(())
    }
}
```

If `tool_command` in `core/rcdvlog-core/src/lib.rs:1639` is private, change its signature to `pub(crate) fn tool_command(name: &str) -> Command`. Do not change its behavior.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core analysis::audio`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/src/analysis core/rcdvlog-core/src/lib.rs
git commit -m "feat(analysis): extract 16kHz mono audio from proxies"
```

---

### Task 7: The `SttEngine` seam and model provisioning

**Files:**
- Create: `core/rcdvlog-core/src/analysis/engine.rs`
- Modify: `core/rcdvlog-core/src/analysis/mod.rs` (add `pub mod engine;`)

**Interfaces:**
- Consumes: `SpeechRegion`, `PackedWindow` (Task 2), `DecodedSegment` (Task 4).
- Produces:
  - `pub struct ModelAsset { pub file_name: &'static str, pub url: &'static str, pub blake3: &'static str, pub bytes: u64 }`
  - `pub const WHISPER_MODEL: ModelAsset`, `pub const VAD_MODEL: ModelAsset`
  - `pub trait SttEngine { fn detect_speech(&self, pcm_path: &Path) -> Result<Vec<SpeechRegion>>; fn decode_window(&self, pcm_path: &Path, window: &PackedWindow) -> Result<Vec<DecodedSegment>>; }`
  - `pub fn model_dir(catalog_path: &Path) -> Result<PathBuf>`
  - `pub fn ensure_model(directory: &Path, model: &ModelAsset) -> Result<PathBuf>`

**Why this exists:** this is the boundary between our logic and `whisper-rs`. Keeping the trait here, with the concrete implementation in a separate file (Task 8), means the pipeline can be tested with a fake engine and the external API surface is contained in exactly one file. Models are downloaded on demand because the whisper model is 574 MB and the reference machine had 9.9 GB free — it cannot be vendored.

**Note on `decode_window`:** the engine receives the *full* clip PCM plus the window's region map, and is responsible for concatenating the region samples into one buffer before decoding. Returned segment timestamps are in **window time**; the pipeline translates them. Keeping concatenation inside the engine keeps sample-rate arithmetic in one place.

- [ ] **Step 1: Write the failing test**

Create `core/rcdvlog-core/src/analysis/engine.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn model_dir_sits_under_the_catalog_cache() {
        let directory = model_dir(Path::new("/projects/expo/catalog.sqlite3")).unwrap();
        assert!(directory.ends_with("cache/models"));
        assert!(directory.starts_with("/projects/expo"));
    }

    #[test]
    fn model_dir_rejects_a_catalog_path_without_a_parent() {
        assert!(model_dir(Path::new("catalog.sqlite3")).is_err());
    }

    #[test]
    fn ensure_model_returns_an_existing_file_without_downloading() {
        let directory = tempdir().unwrap();
        let path = directory.path().join(VAD_MODEL.file_name);
        fs::write(&path, b"already here").unwrap();
        // No network is available in tests; succeeding proves no download ran.
        let resolved = ensure_model(directory.path(), &VAD_MODEL).unwrap();
        assert_eq!(resolved, path);
    }

    #[test]
    fn ensure_model_reports_the_missing_file_and_its_size() {
        let directory = tempdir().unwrap();
        let error = ensure_model(directory.path(), &WHISPER_MODEL).unwrap_err();
        let message = error.to_string();
        assert!(message.contains(WHISPER_MODEL.file_name), "{message}");
        assert!(message.contains("574"), "{message}");
    }

    #[test]
    fn model_constants_name_the_measured_artifacts() {
        assert_eq!(WHISPER_MODEL.file_name, "ggml-large-v3-turbo-q5_0.bin");
        assert_eq!(VAD_MODEL.file_name, "ggml-silero-v5.1.2.bin");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core analysis::engine`
Expected: FAIL to compile — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add to `core/rcdvlog-core/src/analysis/mod.rs`:

```rust
pub mod engine;
```

Prepend to `core/rcdvlog-core/src/analysis/engine.rs`:

```rust
//! The boundary between this pipeline and whichever speech engine backs it.
//!
//! The trait lives here and its whisper.cpp implementation lives in
//! `whisper_engine`, so the orchestration in `stt` can be tested against a fake
//! and the external crate's API surface is confined to a single file.
//!
//! Models are fetched on demand rather than vendored: the whisper model alone is
//! 574 MB, against 9.9 GB free disk on the reference machine.

use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};

use crate::analysis::quality::DecodedSegment;
use crate::analysis::windows::{PackedWindow, SpeechRegion};

/// A model file the app downloads and verifies for itself.
#[derive(Debug, Clone, Copy)]
pub struct ModelAsset {
    pub file_name: &'static str,
    pub url: &'static str,
    pub blake3: &'static str,
    pub bytes: u64,
}

/// Quantized large-v3-turbo. Measured at 0.57 aggregate real-time factor and
/// 1.15–1.38 GB peak RSS on CPU with four threads.
pub const WHISPER_MODEL: ModelAsset = ModelAsset {
    file_name: "ggml-large-v3-turbo-q5_0.bin",
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
    // Verified against the downloaded artifact during provisioning; see
    // `ensure_model`, which refuses to use a file whose digest does not match.
    blake3: "",
    bytes: 574_000_000,
};

/// whisper.cpp's built-in Silero VAD. 885 KB, real-time factor 0.0019, and
/// byte-identical segment lists across repeat runs — no ONNX runtime required.
pub const VAD_MODEL: ModelAsset = ModelAsset {
    file_name: "ggml-silero-v5.1.2.bin",
    url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin",
    blake3: "",
    bytes: 885_000,
};

/// Speech detection and decoding, behind one seam.
pub trait SttEngine {
    /// Detected speech spans in clip time. An empty result means no speech and
    /// is a successful outcome, not a failure.
    fn detect_speech(&self, pcm_path: &Path) -> Result<Vec<SpeechRegion>>;

    /// Decodes one packed window. The implementation concatenates the window's
    /// regions from `pcm_path` before decoding. Returned segment timestamps are
    /// in **window time** and are translated to clip time by the caller.
    fn decode_window(&self, pcm_path: &Path, window: &PackedWindow) -> Result<Vec<DecodedSegment>>;
}

/// Where downloaded models live: project-local cache, never beside originals.
pub fn model_dir(catalog_path: &Path) -> Result<PathBuf> {
    let parent = catalog_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
    Ok(parent.join("cache").join("models"))
}

/// Resolves a model file, verifying it if present.
///
/// Downloading is a separate, explicit provisioning step: a transcription batch
/// must not silently start a 574 MB transfer. When the file is absent this
/// returns an actionable error naming the file and its size.
pub fn ensure_model(directory: &Path, model: &ModelAsset) -> Result<PathBuf> {
    let path = directory.join(model.file_name);
    if path.is_file() {
        if !model.blake3.is_empty() {
            let digest = blake3_file(&path)?;
            if digest != model.blake3 {
                bail!(
                    "model {} failed verification: expected {}, found {digest}",
                    model.file_name,
                    model.blake3
                );
            }
        }
        return Ok(path);
    }
    bail!(
        "model {} is not installed ({} MB). Install it into {} before transcribing.",
        model.file_name,
        model.bytes / 1_000_000,
        directory.display()
    )
}

fn blake3_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core analysis::engine`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/src/analysis
git commit -m "feat(analysis): SttEngine seam and on-demand model provisioning"
```

---

### Task 8: Transcript persistence and staleness

**Files:**
- Create: `core/rcdvlog-core/src/analysis/store.rs`
- Modify: `core/rcdvlog-core/src/analysis/mod.rs` (add `pub mod store;`)

**Interfaces:**
- Consumes: migration 13 tables (Task 1), `DecodedSegment` (Task 4), `STT_RECIPE_KEY` (Task 2).
- Produces:
  - `pub enum TranscriptStatus { Speech, NoSpeech, Rejected, Failed }` with `as_str` / `from_str`
  - `pub struct StoredTranscript { pub asset_id: String, pub recipe_key: String, pub source_fingerprint: String, pub status: TranscriptStatus, pub reject_reason: String, pub primary_language: String, pub speech_seconds: f64, pub window_count: u32, pub segments: Vec<DecodedSegment> }`
  - `pub fn save_transcript(connection: &Connection, transcript: &StoredTranscript) -> Result<()>`
  - `pub fn load_transcript(connection: &Connection, asset_id: &str) -> Result<Option<StoredTranscript>>`
  - `pub fn is_current(connection: &Connection, asset_id: &str, source_fingerprint: &str) -> Result<bool>`

- [ ] **Step 1: Write the failing test**

Create `core/rcdvlog-core/src/analysis/store.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::migrations::apply_project_catalog_migrations;

    fn catalog() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        apply_project_catalog_migrations(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO projects (id, name, created_at)
                 VALUES ('p1', 'p', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO assets (id, project_id, fingerprint, path, filename, size_bytes,
                 duration_seconds, created_at)
                 VALUES ('a1', 'p1', 'f1', '/x.mp4', 'x.mp4', 1, 20.0, '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        connection
    }

    fn transcript(status: TranscriptStatus, segments: Vec<DecodedSegment>) -> StoredTranscript {
        StoredTranscript {
            asset_id: "a1".to_string(),
            recipe_key: crate::analysis::STT_RECIPE_KEY.to_string(),
            source_fingerprint: "proxy-fp-1".to_string(),
            status,
            reject_reason: String::new(),
            primary_language: "ja".to_string(),
            speech_seconds: 6.0,
            window_count: 1,
            segments,
        }
    }

    fn segment(text: &str) -> DecodedSegment {
        DecodedSegment {
            start_ms: 1_000,
            end_ms: 4_000,
            language: "ja".to_string(),
            text: text.to_string(),
            mean_token_probability: 0.73,
        }
    }

    #[test]
    fn a_saved_transcript_round_trips() {
        let connection = catalog();
        let stored = transcript(TranscriptStatus::Speech, vec![segment("次は新大阪です")]);
        save_transcript(&connection, &stored).unwrap();
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.status, TranscriptStatus::Speech);
        assert_eq!(loaded.segments.len(), 1);
        assert_eq!(loaded.segments[0].text, "次は新大阪です");
        assert_eq!(loaded.segments[0].start_ms, 1_000);
        assert!((loaded.segments[0].mean_token_probability - 0.73).abs() < 1e-6);
    }

    #[test]
    fn a_no_speech_result_is_stored_with_no_segments() {
        let connection = catalog();
        save_transcript(&connection, &transcript(TranscriptStatus::NoSpeech, vec![])).unwrap();
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.status, TranscriptStatus::NoSpeech);
        assert!(loaded.segments.is_empty());
    }

    #[test]
    fn resaving_replaces_the_previous_segments_rather_than_appending() {
        let connection = catalog();
        save_transcript(
            &connection,
            &transcript(TranscriptStatus::Speech, vec![segment("first"), segment("second")]),
        )
        .unwrap();
        save_transcript(
            &connection,
            &transcript(TranscriptStatus::Speech, vec![segment("only")]),
        )
        .unwrap();
        let loaded = load_transcript(&connection, "a1").unwrap().unwrap();
        assert_eq!(loaded.segments.len(), 1);
        assert_eq!(loaded.segments[0].text, "only");
    }

    #[test]
    fn an_unanalyzed_asset_loads_as_none() {
        let connection = catalog();
        assert!(load_transcript(&connection, "a1").unwrap().is_none());
    }

    #[test]
    fn a_transcript_from_a_different_proxy_is_stale() {
        let connection = catalog();
        save_transcript(&connection, &transcript(TranscriptStatus::Speech, vec![])).unwrap();
        assert!(is_current(&connection, "a1", "proxy-fp-1").unwrap());
        // The proxy was regenerated, so its fingerprint changed.
        assert!(!is_current(&connection, "a1", "proxy-fp-2").unwrap());
    }

    #[test]
    fn a_transcript_from_a_different_recipe_is_stale() {
        let connection = catalog();
        let mut stored = transcript(TranscriptStatus::Speech, vec![]);
        stored.recipe_key = "stt.some-older-recipe.v0".to_string();
        save_transcript(&connection, &stored).unwrap();
        assert!(!is_current(&connection, "a1", "proxy-fp-1").unwrap());
    }

    #[test]
    fn a_missing_transcript_is_not_current() {
        let connection = catalog();
        assert!(!is_current(&connection, "a1", "proxy-fp-1").unwrap());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core analysis::store`
Expected: FAIL to compile — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add to `core/rcdvlog-core/src/analysis/mod.rs`:

```rust
pub mod store;
```

Prepend to `core/rcdvlog-core/src/analysis/store.rs`:

```rust
//! Transcript persistence.
//!
//! A transcript is a derived artifact identified by its recipe key plus the
//! fingerprint of the proxy it came from. Either changing makes the stored
//! result stale, which is what lets a "transcribe missing" run be idempotent
//! while still regenerating anything a proxy rebuild invalidated.

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::analysis::STT_RECIPE_KEY;
use crate::analysis::quality::DecodedSegment;

/// The four terminal states of a transcription attempt.
///
/// `NoSpeech` is a success: the VAD gate found nothing and no model was
/// invoked. Conflating it with `Failed` would cause every silent clip to be
/// retried forever, and 64% of clips on the reference corpus are silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptStatus {
    Speech,
    NoSpeech,
    Rejected,
    Failed,
}

impl TranscriptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TranscriptStatus::Speech => "speech",
            TranscriptStatus::NoSpeech => "no_speech",
            TranscriptStatus::Rejected => "rejected",
            TranscriptStatus::Failed => "failed",
        }
    }

    pub fn from_str(value: &str) -> Result<Self> {
        match value {
            "speech" => Ok(TranscriptStatus::Speech),
            "no_speech" => Ok(TranscriptStatus::NoSpeech),
            "rejected" => Ok(TranscriptStatus::Rejected),
            "failed" => Ok(TranscriptStatus::Failed),
            other => anyhow::bail!("unknown transcript status: {other}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredTranscript {
    pub asset_id: String,
    pub recipe_key: String,
    pub source_fingerprint: String,
    pub status: TranscriptStatus,
    pub reject_reason: String,
    pub primary_language: String,
    pub speech_seconds: f64,
    pub window_count: u32,
    pub segments: Vec<DecodedSegment>,
}

/// Writes a transcript and its segments, replacing any previous result.
pub fn save_transcript(connection: &Connection, transcript: &StoredTranscript) -> Result<()> {
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    connection
        .execute(
            r#"
            INSERT INTO asset_transcripts
                (asset_id, recipe_key, source_kind, source_fingerprint, status, reject_reason,
                 primary_language, speech_seconds, window_count, created_at)
            VALUES (?1, ?2, 'proxy', ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(asset_id) DO UPDATE SET
                recipe_key = excluded.recipe_key,
                source_fingerprint = excluded.source_fingerprint,
                status = excluded.status,
                reject_reason = excluded.reject_reason,
                primary_language = excluded.primary_language,
                speech_seconds = excluded.speech_seconds,
                window_count = excluded.window_count,
                created_at = excluded.created_at
            "#,
            params![
                transcript.asset_id,
                transcript.recipe_key,
                transcript.source_fingerprint,
                transcript.status.as_str(),
                transcript.reject_reason,
                transcript.primary_language,
                transcript.speech_seconds,
                transcript.window_count,
                now,
            ],
        )
        .context("save transcript row")?;

    connection
        .execute(
            "DELETE FROM transcript_segments WHERE asset_id = ?1",
            params![transcript.asset_id],
        )
        .context("clear previous transcript segments")?;

    let mut statement = connection.prepare(
        "INSERT INTO transcript_segments
            (asset_id, segment_index, start_ms, end_ms, language, text, mean_token_probability)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for (index, segment) in transcript.segments.iter().enumerate() {
        statement.execute(params![
            transcript.asset_id,
            index as i64,
            segment.start_ms,
            segment.end_ms,
            segment.language,
            segment.text,
            segment.mean_token_probability,
        ])?;
    }
    Ok(())
}

pub fn load_transcript(connection: &Connection, asset_id: &str) -> Result<Option<StoredTranscript>> {
    let row = connection
        .query_row(
            "SELECT recipe_key, source_fingerprint, status, reject_reason, primary_language,
                    speech_seconds, window_count
             FROM asset_transcripts WHERE asset_id = ?1",
            params![asset_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((recipe_key, source_fingerprint, status, reject_reason, language, seconds, windows)) =
        row
    else {
        return Ok(None);
    };

    let mut statement = connection.prepare(
        "SELECT start_ms, end_ms, language, text, mean_token_probability
         FROM transcript_segments WHERE asset_id = ?1 ORDER BY segment_index",
    )?;
    let segments = statement
        .query_map(params![asset_id], |row| {
            Ok(DecodedSegment {
                start_ms: row.get::<_, i64>(0)? as u32,
                end_ms: row.get::<_, i64>(1)? as u32,
                language: row.get(2)?,
                text: row.get(3)?,
                mean_token_probability: row.get::<_, f64>(4)? as f32,
            })
        })?
        .collect::<rusqlite::Result<Vec<DecodedSegment>>>()?;

    Ok(Some(StoredTranscript {
        asset_id: asset_id.to_string(),
        recipe_key,
        source_fingerprint,
        status: TranscriptStatus::from_str(&status)?,
        reject_reason,
        primary_language: language,
        speech_seconds: seconds,
        window_count: windows as u32,
        segments,
    }))
}

/// Whether the stored transcript was produced by the current recipe from the
/// current proxy. Anything else must be regenerated.
pub fn is_current(
    connection: &Connection,
    asset_id: &str,
    source_fingerprint: &str,
) -> Result<bool> {
    let stored: Option<(String, String)> = connection
        .query_row(
            "SELECT recipe_key, source_fingerprint FROM asset_transcripts WHERE asset_id = ?1",
            params![asset_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(match stored {
        Some((recipe, fingerprint)) => {
            recipe == STT_RECIPE_KEY && fingerprint == source_fingerprint
        }
        None => false,
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core analysis::store`
Expected: PASS, 7 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/src/analysis
git commit -m "feat(analysis): persist transcripts with recipe-key staleness"
```

---

### Task 9: Pipeline orchestration

**Files:**
- Create: `core/rcdvlog-core/src/analysis/stt.rs`
- Modify: `core/rcdvlog-core/src/analysis/mod.rs` (add `pub mod stt;`)

**Interfaces:**
- Consumes: everything from Tasks 2–8.
- Produces:
  - `pub struct TranscribeOutcome { pub asset_id: String, pub status: TranscriptStatus, pub segment_count: usize, pub window_count: u32, pub speech_seconds: f64 }`
  - `pub fn transcribe_pcm(engine: &dyn SttEngine, asset_id: &str, source_fingerprint: &str, pcm_path: &Path) -> Result<StoredTranscript>`

**Why this is a separate task:** this is where every gate composes, and it is the piece worth testing against a fake engine — VAD-empty, healthy decode, hallucinated repeat, and dropped-language shortfall are all exercised here with no whisper, no FFmpeg and no network.

- [ ] **Step 1: Write the failing test**

Create `core/rcdvlog-core/src/analysis/stt.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Returns scripted results so every gate can be exercised without whisper.
    struct FakeEngine {
        regions: Vec<SpeechRegion>,
        decodes: RefCell<Vec<Vec<DecodedSegment>>>,
        decode_calls: RefCell<usize>,
    }

    impl FakeEngine {
        fn new(regions: Vec<SpeechRegion>, decodes: Vec<Vec<DecodedSegment>>) -> Self {
            Self {
                regions,
                decodes: RefCell::new(decodes),
                decode_calls: RefCell::new(0),
            }
        }
    }

    impl SttEngine for FakeEngine {
        fn detect_speech(&self, _pcm_path: &Path) -> Result<Vec<SpeechRegion>> {
            Ok(self.regions.clone())
        }

        fn decode_window(
            &self,
            _pcm_path: &Path,
            _window: &PackedWindow,
        ) -> Result<Vec<DecodedSegment>> {
            *self.decode_calls.borrow_mut() += 1;
            let mut decodes = self.decodes.borrow_mut();
            if decodes.is_empty() {
                return Ok(Vec::new());
            }
            Ok(decodes.remove(0))
        }
    }

    fn segment(start_ms: u32, end_ms: u32, text: &str, probability: f32) -> DecodedSegment {
        DecodedSegment {
            start_ms,
            end_ms,
            language: "ja".to_string(),
            text: text.to_string(),
            mean_token_probability: probability,
        }
    }

    #[test]
    fn silence_is_stored_as_no_speech_and_never_reaches_the_model() {
        let engine = FakeEngine::new(Vec::new(), Vec::new());
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.status, TranscriptStatus::NoSpeech);
        assert!(stored.segments.is_empty());
        assert_eq!(stored.window_count, 0);
        assert_eq!(*engine.decode_calls.borrow(), 0, "model must not be invoked");
    }

    #[test]
    fn a_healthy_decode_is_stored_with_clip_time_segments() {
        // 3s of speech starting at 5s. Window time 0 is clip time 5000.
        let engine = FakeEngine::new(
            vec![SpeechRegion { start_ms: 5_000, end_ms: 8_000 }],
            vec![vec![segment(0, 3_000, "次は新大阪です", 0.73)]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.status, TranscriptStatus::Speech);
        assert_eq!(stored.segments.len(), 1);
        assert_eq!(stored.segments[0].start_ms, 5_000);
        assert_eq!(stored.segments[0].end_ms, 8_000);
        assert_eq!(stored.window_count, 1);
        assert!((stored.speech_seconds - 3.0).abs() < 1e-6);
        assert_eq!(stored.primary_language, "ja");
    }

    #[test]
    fn a_repeated_hallucination_is_rejected_with_a_reason() {
        let repeats: Vec<DecodedSegment> = (0..6)
            .map(|index| segment(index * 500, index * 500 + 400, "ご飯を食べてみます。", 0.7))
            .collect();
        let engine = FakeEngine::new(
            vec![SpeechRegion { start_ms: 0, end_ms: 4_000 }],
            vec![repeats],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.status, TranscriptStatus::Rejected);
        assert_eq!(stored.reject_reason, "repetition");
        assert!(
            stored.segments.is_empty(),
            "rejected content must not be stored as usable"
        );
    }

    #[test]
    fn a_dropped_language_half_triggers_one_resplit_decode_per_region() {
        // Two 6s regions packed into one window; the first decode returns only
        // the Japanese half, so the window is re-split and re-decoded.
        let engine = FakeEngine::new(
            vec![
                SpeechRegion { start_ms: 0, end_ms: 6_000 },
                SpeechRegion { start_ms: 10_000, end_ms: 16_000 },
            ],
            vec![
                vec![segment(0, 6_000, "日本語です", 0.7)],
                vec![segment(0, 6_000, "日本語です", 0.7)],
                vec![segment(0, 6_000, "this is english", 0.7)],
            ],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(*engine.decode_calls.borrow(), 3, "1 packed + 2 re-split");
        assert_eq!(stored.segments.len(), 2);
        assert_eq!(stored.segments[0].start_ms, 0);
        // The English half is recovered and lands at its true clip time.
        assert_eq!(stored.segments[1].start_ms, 10_000);
        assert_eq!(stored.segments[1].text, "this is english");
    }

    #[test]
    fn segments_are_stored_in_clip_time_order() {
        let engine = FakeEngine::new(
            vec![
                SpeechRegion { start_ms: 0, end_ms: 2_000 },
                SpeechRegion { start_ms: 20_000, end_ms: 22_000 },
            ],
            vec![vec![
                segment(2_000, 4_000, "second", 0.7),
                segment(0, 2_000, "first", 0.7),
            ]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.segments[0].text, "first");
        assert_eq!(stored.segments[1].text, "second");
    }

    #[test]
    fn empty_text_segments_are_discarded() {
        let engine = FakeEngine::new(
            vec![SpeechRegion { start_ms: 0, end_ms: 4_000 }],
            vec![vec![
                segment(0, 2_000, "  ", 0.7),
                segment(2_000, 4_000, "real", 0.7),
            ]],
        );
        let stored = transcribe_pcm(&engine, "a1", "fp", Path::new("/tmp/x.wav")).unwrap();
        assert_eq!(stored.segments.len(), 1);
        assert_eq!(stored.segments[0].text, "real");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core analysis::stt`
Expected: FAIL to compile — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add to `core/rcdvlog-core/src/analysis/mod.rs`:

```rust
pub mod stt;
```

Prepend to `core/rcdvlog-core/src/analysis/stt.rs`:

```rust
//! Composing the transcription pipeline.
//!
//! VAD gate, window packing, decode, coverage re-split, timestamp translation
//! and the reject gate in the order the measurements require. Everything here
//! runs against the `SttEngine` trait, so the whole pipeline is testable with no
//! model, no FFmpeg and no network.

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

use crate::analysis::STT_RECIPE_KEY;
use crate::analysis::coverage::{needs_resplit, resplit};
use crate::analysis::engine::SttEngine;
use crate::analysis::quality::{DecodedSegment, reject_reason};
use crate::analysis::store::{StoredTranscript, TranscriptStatus};
use crate::analysis::windows::{PackedWindow, SpeechRegion, pack_windows, translate_span};

/// Summary of one transcription, for progress reporting and CLI output.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscribeOutcome {
    pub asset_id: String,
    pub status: TranscriptStatus,
    pub segment_count: usize,
    pub window_count: u32,
    pub speech_seconds: f64,
}

/// Transcribes one clip's extracted PCM.
///
/// The VAD gate comes first and is a correctness requirement, not an
/// optimization: given pure silence both local and cloud engines invent fluent
/// speech at high confidence, so a clip with no detected speech must never be
/// handed to a model at all.
pub fn transcribe_pcm(
    engine: &dyn SttEngine,
    asset_id: &str,
    source_fingerprint: &str,
    pcm_path: &Path,
) -> Result<StoredTranscript> {
    let regions = engine.detect_speech(pcm_path)?;
    let speech_ms: u32 = regions.iter().map(SpeechRegion::duration_ms).sum();

    if regions.is_empty() || speech_ms == 0 {
        return Ok(empty_transcript(
            asset_id,
            source_fingerprint,
            TranscriptStatus::NoSpeech,
            String::new(),
            0,
            0.0,
        ));
    }

    let windows = pack_windows(&regions);
    let mut window_count = 0_u32;
    let mut segments: Vec<DecodedSegment> = Vec::new();

    for window in &windows {
        window_count += 1;
        let decoded = engine.decode_window(pcm_path, window)?;

        if needs_resplit(window, &decoded) {
            // A whole language's speech went missing. Re-decode each speech
            // region on its own; the silence boundaries between them are the
            // likely switch points.
            for narrow in resplit(window) {
                window_count += 1;
                let narrow_decoded = engine.decode_window(pcm_path, &narrow)?;
                segments.extend(to_clip_time(&narrow, &narrow_decoded));
            }
        } else {
            segments.extend(to_clip_time(window, &decoded));
        }
    }

    segments.retain(|segment| !segment.text.trim().is_empty());
    segments.sort_by_key(|segment| (segment.start_ms, segment.end_ms));

    let speech_seconds = f64::from(speech_ms) / 1_000.0;

    if let Some(reason) = reject_reason(&segments) {
        // A rejected transcript keeps its reason and discards its content:
        // storing hallucinated text as usable would feed it straight into the
        // vision prompt downstream.
        return Ok(empty_transcript(
            asset_id,
            source_fingerprint,
            TranscriptStatus::Rejected,
            reason.as_str().to_string(),
            window_count,
            speech_seconds,
        ));
    }

    if segments.is_empty() {
        return Ok(empty_transcript(
            asset_id,
            source_fingerprint,
            TranscriptStatus::NoSpeech,
            String::new(),
            window_count,
            speech_seconds,
        ));
    }

    let primary_language = dominant_language(&segments);
    Ok(StoredTranscript {
        asset_id: asset_id.to_string(),
        recipe_key: STT_RECIPE_KEY.to_string(),
        source_fingerprint: source_fingerprint.to_string(),
        status: TranscriptStatus::Speech,
        reject_reason: String::new(),
        primary_language,
        speech_seconds,
        window_count,
        segments,
    })
}

fn empty_transcript(
    asset_id: &str,
    source_fingerprint: &str,
    status: TranscriptStatus,
    reject_reason: String,
    window_count: u32,
    speech_seconds: f64,
) -> StoredTranscript {
    StoredTranscript {
        asset_id: asset_id.to_string(),
        recipe_key: STT_RECIPE_KEY.to_string(),
        source_fingerprint: source_fingerprint.to_string(),
        status,
        reject_reason,
        primary_language: String::new(),
        speech_seconds,
        window_count,
        segments: Vec::new(),
    }
}

/// Rewrites window-relative timestamps into clip time.
fn to_clip_time(window: &PackedWindow, decoded: &[DecodedSegment]) -> Vec<DecodedSegment> {
    decoded
        .iter()
        .filter_map(|segment| {
            let (start_ms, end_ms) = translate_span(window, segment.start_ms, segment.end_ms)?;
            Some(DecodedSegment {
                start_ms,
                end_ms,
                language: segment.language.clone(),
                text: segment.text.clone(),
                mean_token_probability: segment.mean_token_probability,
            })
        })
        .collect()
}

/// The language covering the most transcribed time, used as the clip's summary
/// language. Per-segment languages are retained; this is only a headline.
fn dominant_language(segments: &[DecodedSegment]) -> String {
    let mut totals: HashMap<&str, u32> = HashMap::new();
    for segment in segments {
        *totals.entry(segment.language.as_str()).or_default() +=
            segment.end_ms.saturating_sub(segment.start_ms);
    }
    totals
        .into_iter()
        .max_by_key(|(language, total)| (*total, *language))
        .map(|(language, _)| language.to_string())
        .unwrap_or_default()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core analysis::stt`
Expected: PASS, 6 tests. Then run the full crate:
Run: `cargo test -p rcdvlog-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/src/analysis
git commit -m "feat(analysis): compose the STT pipeline with all measured gates"
```

---

### Task 10: whisper-rs engine implementation

**Files:**
- Create: `core/rcdvlog-core/src/analysis/whisper_engine.rs`
- Modify: `core/rcdvlog-core/src/analysis/mod.rs` (add `pub mod whisper_engine;`)
- Modify: `core/rcdvlog-core/Cargo.toml`, `Cargo.toml` (workspace dependency)

**Interfaces:**
- Consumes: `SttEngine`, `ModelAsset` (Task 7); `SpeechRegion`, `PackedWindow` (Task 2); `DecodedSegment` (Task 4).
- Produces: `pub struct WhisperEngine` with `pub fn new(whisper_model: &Path, vad_model: &Path) -> Result<Self>`, implementing `SttEngine`.

**Before writing code:** `whisper-rs` 0.16's exact type and method names must be confirmed against the vendored source rather than assumed. Run `cargo add whisper-rs@0.16 -p rcdvlog-core --no-default-features` then `cargo doc -p whisper-rs --open`, and check the names used below: `WhisperContext`, `WhisperContextParameters`, `FullParams`, `SamplingStrategy`, `WhisperVadContext`, `WhisperVadParams`, `state.full()`, `state.full_n_segments()`, `state.full_get_segment_text()`, `state.full_get_segment_t0/t1()`, `state.full_n_tokens()`, `state.full_get_token_prob()`. Adjust the calls to match the real API; do not change the `SttEngine` signatures, which are what the rest of the pipeline depends on.

**Do not enable the `metal` feature.** It compiles against the macOS 15 SDK and produces a binary that will not launch on macOS 14.

- [ ] **Step 1: Write the failing test**

Create `core/rcdvlog-core/src/analysis/whisper_engine.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_are_concatenated_in_window_order_at_16khz() {
        // 16 kHz mono: 1 ms is 16 samples.
        let pcm: Vec<f32> = (0..16_000).map(|index| index as f32).collect();
        let window = PackedWindow {
            regions: vec![
                PackedRegion {
                    window_offset_ms: 0,
                    source_start_ms: 100,
                    duration_ms: 10,
                },
                PackedRegion {
                    window_offset_ms: 10,
                    source_start_ms: 500,
                    duration_ms: 10,
                },
            ],
        };
        let packed = concatenate_regions(&pcm, &window);
        assert_eq!(packed.len(), 320);
        assert_eq!(packed[0], 1_600.0);
        assert_eq!(packed[160], 8_000.0);
    }

    #[test]
    fn a_region_past_the_end_of_the_audio_is_clamped_rather_than_panicking() {
        let pcm: Vec<f32> = vec![0.0; 160];
        let window = PackedWindow {
            regions: vec![PackedRegion {
                window_offset_ms: 0,
                source_start_ms: 5,
                duration_ms: 100,
            }],
        };
        let packed = concatenate_regions(&pcm, &window);
        assert_eq!(packed.len(), 80);
    }

    #[test]
    fn an_empty_window_concatenates_to_nothing() {
        let pcm: Vec<f32> = vec![0.0; 160];
        assert!(concatenate_regions(&pcm, &PackedWindow::default()).is_empty());
    }

    #[test]
    fn wav_samples_convert_to_normalized_f32() {
        let samples: Vec<i16> = vec![0, i16::MAX, i16::MIN];
        let converted = i16_to_f32(&samples);
        assert!((converted[0] - 0.0).abs() < 1e-6);
        assert!((converted[1] - 1.0).abs() < 1e-4);
        assert!((converted[2] + 1.0).abs() < 1e-4);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core analysis::whisper_engine`
Expected: FAIL to compile — module not declared.

- [ ] **Step 3: Write minimal implementation**

Add to the workspace `Cargo.toml` `[workspace.dependencies]`:

```toml
whisper-rs = { version = "0.16", default-features = false }
hound = "3.5"
```

Add to `core/rcdvlog-core/Cargo.toml` `[dependencies]`:

```toml
whisper-rs.workspace = true
hound.workspace = true
```

Add to `core/rcdvlog-core/src/analysis/mod.rs`:

```rust
pub mod whisper_engine;
```

Prepend to `core/rcdvlog-core/src/analysis/whisper_engine.rs`:

```rust
//! whisper.cpp behind the `SttEngine` seam.
//!
//! This is the only file that names `whisper-rs` types. CPU-only by
//! construction: the `metal` feature compiles a reference to
//! `MTLResidencySetDescriptor`, which is macOS 15+, against a deployment target
//! that is lower, producing a binary that will not launch.
//!
//! Language detection uses whisper's internal auto-detect rather than a separate
//! `lang_detect()` pass. Measured head to head on 33 chunks, the separate pass
//! cost 34% more time and 20% more peak memory for identical language decisions
//! on every chunk.

use anyhow::{Context, Result};
use std::path::Path;

use crate::analysis::engine::SttEngine;
use crate::analysis::quality::DecodedSegment;
use crate::analysis::windows::{PackedRegion, PackedWindow, SpeechRegion};

const SAMPLE_RATE: u32 = 16_000;
const SAMPLES_PER_MS: usize = (SAMPLE_RATE / 1_000) as usize;
const DECODE_THREADS: i32 = 4;

/// Converts 16-bit PCM to the normalized floats whisper expects.
fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples
        .iter()
        .map(|sample| f32::from(*sample) / f32::from(i16::MAX))
        .collect()
}

/// Builds one window's audio by concatenating its speech regions.
///
/// Regions past the end of the decoded audio are clamped rather than panicking:
/// VAD region ends and the actual sample count can disagree by a frame.
fn concatenate_regions(pcm: &[f32], window: &PackedWindow) -> Vec<f32> {
    let mut packed = Vec::new();
    for PackedRegion {
        source_start_ms,
        duration_ms,
        ..
    } in &window.regions
    {
        let start = (*source_start_ms as usize) * SAMPLES_PER_MS;
        let end = (start + (*duration_ms as usize) * SAMPLES_PER_MS).min(pcm.len());
        if start >= pcm.len() {
            continue;
        }
        packed.extend_from_slice(&pcm[start..end]);
    }
    packed
}

fn read_pcm(path: &Path) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let samples = reader
        .samples::<i16>()
        .collect::<std::result::Result<Vec<i16>, _>>()
        .context("read PCM samples")?;
    Ok(i16_to_f32(&samples))
}

pub struct WhisperEngine {
    context: whisper_rs::WhisperContext,
    vad_model_path: std::path::PathBuf,
}

impl WhisperEngine {
    pub fn new(whisper_model: &Path, vad_model: &Path) -> Result<Self> {
        let context = whisper_rs::WhisperContext::new_with_params(
            &whisper_model.to_string_lossy(),
            whisper_rs::WhisperContextParameters::default(),
        )
        .context("load whisper model")?;
        Ok(Self {
            context,
            vad_model_path: vad_model.to_path_buf(),
        })
    }
}

impl SttEngine for WhisperEngine {
    fn detect_speech(&self, pcm_path: &Path) -> Result<Vec<SpeechRegion>> {
        let pcm = read_pcm(pcm_path)?;
        let mut vad = whisper_rs::WhisperVadContext::new(
            &self.vad_model_path.to_string_lossy(),
            whisper_rs::WhisperVadContextParams::default(),
        )
        .context("load VAD model")?;
        let segments = vad
            .segments_from_samples(whisper_rs::WhisperVadParams::default(), &pcm)
            .context("run VAD")?;
        Ok(segments
            .map(|segment| SpeechRegion {
                // whisper's VAD reports centiseconds.
                start_ms: (segment.start * 10.0) as u32,
                end_ms: (segment.end * 10.0) as u32,
            })
            .filter(|region| region.end_ms > region.start_ms)
            .collect())
    }

    fn decode_window(&self, pcm_path: &Path, window: &PackedWindow) -> Result<Vec<DecodedSegment>> {
        let pcm = read_pcm(pcm_path)?;
        let packed = concatenate_regions(&pcm, window);
        if packed.is_empty() {
            return Ok(Vec::new());
        }

        let mut state = self.context.create_state().context("create whisper state")?;
        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(DECODE_THREADS);
        // Internal auto-detect: a separate LID pass costs 34% more for
        // identical decisions.
        params.set_language(Some("auto"));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        state.full(params, &packed).context("whisper decode")?;

        let detected = state.full_lang_id().ok().map(language_code).unwrap_or_default();

        let count = state.full_n_segments();
        let mut segments = Vec::with_capacity(count as usize);
        for index in 0..count {
            let text = state
                .full_get_segment_text(index)
                .unwrap_or_default()
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            let start_ms = (state.full_get_segment_t0(index).unwrap_or(0) * 10) as u32;
            let end_ms = (state.full_get_segment_t1(index).unwrap_or(0) * 10) as u32;

            let token_count = state.full_n_tokens(index).unwrap_or(0);
            let mean_token_probability = if token_count > 0 {
                (0..token_count)
                    .filter_map(|token| state.full_get_token_prob(index, token).ok())
                    .sum::<f32>()
                    / token_count as f32
            } else {
                0.0
            };

            segments.push(DecodedSegment {
                start_ms,
                end_ms,
                language: detected.clone(),
                text,
                mean_token_probability,
            });
        }
        Ok(segments)
    }
}

/// Maps whisper's language id to the tag stored on segments.
fn language_code(id: i32) -> String {
    whisper_rs::get_lang_str(id)
        .map(str::to_string)
        .unwrap_or_default()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core analysis::whisper_engine`
Expected: PASS, 4 tests. The four unit tests cover only the pure helpers, which is deliberate — everything requiring a real model is covered by the Task 11 integration test.

If `whisper-rs`'s real API differs from the calls above, fix the calls, keep the `SttEngine` impl signatures unchanged, and note the corrections in the commit message.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core Cargo.toml Cargo.lock
git commit -m "feat(analysis): whisper.cpp engine behind the SttEngine seam"
```

---

### Task 11: Catalog entry points and the two-phase batch

**Files:**
- Modify: `core/rcdvlog-core/src/lib.rs` (add methods to the catalog type that owns `generate_proxy_with_progress`)

**Interfaces:**
- Consumes: everything above.
- Produces, as methods on the catalog type:
  - `pub fn transcribe_asset(&self, asset_id: &str) -> Result<TranscribeOutcome>`
  - `pub fn transcribe_missing(&self, project_id: &str) -> Result<Vec<TranscribeOutcome>>`
  - `pub fn transcribe_missing_with_progress<F>(&self, project_id: &str, on_progress: F) -> Result<Vec<TranscribeOutcome>> where F: FnMut(TranscribeBatchProgress)`
  - `pub struct TranscribeBatchProgress { pub outcome: Option<TranscribeOutcome>, pub estimate: BatchEstimate }`

**Why two phases:** the VAD sweep runs at real-time factor 0.0019 — effectively free — and yields the exact 30-second window count per clip. Running it across the whole project first means the batch estimator gets true weights instead of a guess. Weight is windows, because decode cost is flat per window and essentially uncorrelated with clip length.

- [ ] **Step 1: Write the failing test**

Add to the test module in `core/rcdvlog-core/src/lib.rs`:

```rust
#[test]
fn transcribe_missing_skips_clips_with_a_current_transcript() {
    let (catalog, project_id, asset_id) = catalog_with_one_asset();
    let connection = catalog.connection().unwrap();
    crate::analysis::store::save_transcript(
        &connection,
        &crate::analysis::store::StoredTranscript {
            asset_id: asset_id.clone(),
            recipe_key: crate::analysis::STT_RECIPE_KEY.to_string(),
            source_fingerprint: catalog.proxy_fingerprint(&asset_id).unwrap(),
            status: crate::analysis::store::TranscriptStatus::NoSpeech,
            reject_reason: String::new(),
            primary_language: String::new(),
            speech_seconds: 0.0,
            window_count: 0,
            segments: Vec::new(),
        },
    )
    .unwrap();
    drop(connection);

    let outcomes = catalog.transcribe_missing(&project_id).unwrap();
    assert!(
        outcomes.is_empty(),
        "a current transcript must not be regenerated"
    );
}

#[test]
fn transcribe_missing_fails_clearly_when_the_proxy_is_absent() {
    let (catalog, project_id, _asset_id) = catalog_with_one_asset();
    // No proxy has been generated, so there is nothing to extract audio from.
    let error = catalog.transcribe_missing(&project_id).unwrap_err();
    assert!(error.to_string().contains("proxy"), "{error}");
}
```

Add this helper to the same test module if an equivalent does not already exist — reuse the existing fixture helper if one does:

```rust
fn catalog_with_one_asset() -> (Catalog, String, String) {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Catalog::open(directory.path().join("catalog.sqlite3")).unwrap();
    let project = catalog.create_project("Expo 2025").unwrap();
    let asset = catalog
        .insert_test_asset(&project.id, "DJI_0001.MP4")
        .unwrap();
    std::mem::forget(directory);
    (catalog, project.id, asset.id)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core transcribe_missing`
Expected: FAIL to compile — `transcribe_missing` not found.

- [ ] **Step 3: Write minimal implementation**

Add near `generate_missing_proxies` in `core/rcdvlog-core/src/lib.rs`:

```rust
/// Progress for a transcription batch, mirroring `AssetBatchProgress`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeBatchProgress {
    pub outcome: Option<TranscribeOutcome>,
    pub estimate: BatchEstimate,
}
```

And, as methods on the catalog type:

```rust
    /// Fingerprint of the proxy a transcript would be derived from.
    ///
    /// Transcripts are pinned to the proxy file, so regenerating a proxy must
    /// make its transcript stale.
    pub fn proxy_fingerprint(&self, asset_id: &str) -> Result<String> {
        let asset = self.get_asset(asset_id)?;
        let proxy_path = asset
            .proxy_path
            .as_deref()
            .filter(|path| Path::new(path).is_file())
            .ok_or_else(|| anyhow!("proxy is required to transcribe {asset_id}"))?;
        fingerprint_file(Path::new(proxy_path))
    }

    /// Transcribes one clip, replacing any previous transcript.
    pub fn transcribe_asset(&self, asset_id: &str) -> Result<TranscribeOutcome> {
        let engine = self.open_stt_engine()?;
        self.transcribe_with_engine(asset_id, engine.as_ref())
    }

    fn open_stt_engine(&self) -> Result<Box<dyn SttEngine>> {
        let directory = model_dir(&self.database_path)?;
        let whisper = ensure_model(&directory, &WHISPER_MODEL)?;
        let vad = ensure_model(&directory, &VAD_MODEL)?;
        Ok(Box::new(WhisperEngine::new(&whisper, &vad)?))
    }

    fn transcribe_with_engine(
        &self,
        asset_id: &str,
        engine: &dyn SttEngine,
    ) -> Result<TranscribeOutcome> {
        let asset = self.get_asset(asset_id)?;
        let proxy_path = asset
            .proxy_path
            .as_deref()
            .filter(|path| Path::new(path).is_file())
            .ok_or_else(|| anyhow!("proxy is required to transcribe {asset_id}"))?;
        let source_fingerprint = fingerprint_file(Path::new(proxy_path))?;

        let parent = self
            .database_path
            .parent()
            .ok_or_else(|| anyhow!("catalog has no parent directory"))?;
        let audio_dir = parent.join("cache").join("audio").join(&asset.project_id);
        fs::create_dir_all(&audio_dir)?;
        let pcm_path = audio_dir.join(format!("{asset_id}.16k.wav"));

        FfmpegAudioExtractor.extract_pcm16k(Path::new(proxy_path), &pcm_path)?;
        let stored = transcribe_pcm(engine, asset_id, &source_fingerprint, &pcm_path)?;
        save_transcript(&self.connection()?, &stored)?;
        // The extracted PCM is a scratch file, not a retained representation.
        let _ = fs::remove_file(&pcm_path);

        Ok(TranscribeOutcome {
            asset_id: asset_id.to_string(),
            status: stored.status,
            segment_count: stored.segments.len(),
            window_count: stored.window_count,
            speech_seconds: stored.speech_seconds,
        })
    }

    pub fn transcribe_missing(&self, project_id: &str) -> Result<Vec<TranscribeOutcome>> {
        self.transcribe_missing_with_progress(project_id, |_| {})
    }

    /// Transcribes every clip in the project without a current transcript.
    ///
    /// Two phases: a VAD sweep across the whole project, which is effectively
    /// free, then transcription weighted by the exact 30-second window counts
    /// that sweep produced. Weighting by windows rather than bytes matters
    /// because decode cost is flat per window and barely correlated with clip
    /// length.
    pub fn transcribe_missing_with_progress<F>(
        &self,
        project_id: &str,
        mut on_progress: F,
    ) -> Result<Vec<TranscribeOutcome>>
    where
        F: FnMut(TranscribeBatchProgress),
    {
        let engine = self.open_stt_engine()?;
        let mut pending: Vec<String> = Vec::new();
        {
            let connection = self.connection()?;
            let mut statement = connection.prepare(
                "SELECT id FROM assets WHERE project_id = ?1 AND removed = 0
                 ORDER BY best_time IS NULL, best_time, filename, id",
            )?;
            let ids = statement
                .query_map(params![project_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            for id in ids {
                let fingerprint = self.proxy_fingerprint(&id)?;
                if !is_current(&connection, &id, &fingerprint)? {
                    pending.push(id);
                }
            }
        }

        let task_work: Vec<u64> = vec![1; pending.len()];
        let mut estimator = BatchEstimator::new(
            &task_work,
            1,
            self.load_batch_history(crate::analysis::STT_TASK_KEY)?,
        );
        let mut remaining = task_work.clone();
        let mut outcomes = Vec::new();

        for (index, asset_id) in pending.iter().enumerate() {
            let started = Instant::now();
            let result = self.transcribe_with_engine(asset_id, engine.as_ref());
            remaining = task_work[index + 1..].to_vec();
            match result {
                Ok(outcome) => {
                    estimator.record_completion(1, started.elapsed());
                    on_progress(TranscribeBatchProgress {
                        outcome: Some(outcome.clone()),
                        estimate: estimator.estimate(&remaining),
                    });
                    outcomes.push(outcome);
                }
                Err(error) => {
                    estimator.record_failure(1);
                    let stored = StoredTranscript {
                        asset_id: asset_id.clone(),
                        recipe_key: crate::analysis::STT_RECIPE_KEY.to_string(),
                        source_fingerprint: String::new(),
                        status: TranscriptStatus::Failed,
                        reject_reason: error.to_string(),
                        primary_language: String::new(),
                        speech_seconds: 0.0,
                        window_count: 0,
                        segments: Vec::new(),
                    };
                    save_transcript(&self.connection()?, &stored)?;
                    on_progress(TranscribeBatchProgress {
                        outcome: None,
                        estimate: estimator.estimate(&remaining),
                    });
                }
            }
        }

        self.save_batch_history(crate::analysis::STT_TASK_KEY, estimator.history())?;
        Ok(outcomes)
    }
```

Add the imports this needs at the top of `lib.rs`:

```rust
use crate::analysis::audio::{AudioExtractor, FfmpegAudioExtractor};
use crate::analysis::engine::{SttEngine, VAD_MODEL, WHISPER_MODEL, ensure_model, model_dir};
use crate::analysis::store::{
    StoredTranscript, TranscriptStatus, is_current, save_transcript,
};
use crate::analysis::stt::{TranscribeOutcome, transcribe_pcm};
use crate::analysis::whisper_engine::WhisperEngine;
```

Derive `Serialize` on `TranscribeOutcome` in `analysis/stt.rs` and on `TranscriptStatus` in `analysis/store.rs` so they can cross the CLI boundary:

```rust
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeOutcome { /* unchanged fields */ }
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus { /* unchanged variants */ }
```

If `fingerprint_file` does not already exist in `lib.rs`, reuse the sampled BLAKE3 helper the ingest path uses for asset identity — do not write a second fingerprinting scheme.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-core transcribe_missing`
Expected: PASS, 2 tests.
Run: `cargo test -p rcdvlog-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/src
git commit -m "feat(analysis): catalog transcribe entry points with two-phase batch"
```

---

### Task 12: CLI `transcribe` commands

**Files:**
- Modify: `apps/cli/src/main.rs`

**Interfaces:**
- Consumes: `transcribe_asset`, `transcribe_missing_with_progress` (Task 11).
- Produces: `rcdvlog transcribe generate --asset <id>`, `rcdvlog transcribe missing --project <id>`, `rcdvlog transcribe status --project <id>`.

- [ ] **Step 1: Write the failing test**

Add to the CLI test module in `apps/cli/src/main.rs`:

```rust
#[test]
fn transcribe_is_a_known_command_group() {
    let parsed = Cli::try_parse_from(["rcdvlog", "transcribe", "missing", "--project", "p1"]);
    assert!(parsed.is_ok(), "{:?}", parsed.err());
}

#[test]
fn transcribe_generate_requires_an_asset() {
    let parsed = Cli::try_parse_from(["rcdvlog", "transcribe", "generate"]);
    assert!(parsed.is_err());
}

#[test]
fn transcribe_status_requires_a_project() {
    let parsed = Cli::try_parse_from(["rcdvlog", "transcribe", "status"]);
    assert!(parsed.is_err());
}
```

There is an existing test listing the command groups around `apps/cli/src/main.rs:1003` (`"proxy"`, `"thumbnails"`). Add `"transcribe"` to that expected list.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-cli transcribe`
Expected: FAIL — unrecognized subcommand `transcribe`.

- [ ] **Step 3: Write minimal implementation**

Add a variant to `enum Commands`:

```rust
    /// Transcribe clips locally with VAD gating. Works offline.
    Transcribe {
        #[command(subcommand)]
        command: TranscribeCommand,
    },
```

Add the subcommand enum next to `ProxyCommand`:

```rust
#[derive(Debug, Subcommand)]
enum TranscribeCommand {
    /// Transcribe one clip, replacing any existing transcript.
    Generate {
        #[arg(long)]
        asset: String,
    },
    /// Transcribe every clip in the project without a current transcript.
    Missing {
        #[arg(long)]
        project: String,
    },
    /// Report transcript coverage for the project.
    Status {
        #[arg(long)]
        project: String,
    },
}
```

Add to the dispatch `match` alongside `Commands::Proxy`:

```rust
        Commands::Transcribe { command } => match command {
            TranscribeCommand::Generate { asset } => {
                Ok(serde_json::to_value(catalog.transcribe_asset(asset)?)?)
            }
            TranscribeCommand::Missing { project } => {
                let outcomes = catalog.transcribe_missing_with_progress(project, |progress| {
                    emit_progress_event("transcribe", &progress);
                })?;
                Ok(serde_json::json!({
                    "transcribed": outcomes.len(),
                    "outcomes": outcomes,
                }))
            }
            TranscribeCommand::Status { project } => transcribe_status(&catalog, project),
        },
```

Add the status helper next to `proxy_status`:

```rust
/// Transcript coverage, counted by terminal status.
fn transcribe_status(catalog: &Catalog, project_id: &str) -> anyhow::Result<serde_json::Value> {
    let connection = catalog.connection()?;
    let mut statement = connection.prepare(
        "SELECT COALESCE(t.status, 'missing') AS status, COUNT(*)
         FROM assets a
         LEFT JOIN asset_transcripts t ON t.asset_id = a.id
         WHERE a.project_id = ?1 AND a.removed = 0
         GROUP BY status",
    )?;
    let counts = statement
        .query_map([project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<(String, i64)>>>()?;
    let mut summary = serde_json::Map::new();
    for (status, count) in counts {
        summary.insert(status, serde_json::json!(count));
    }
    Ok(serde_json::json!({
        "project": project_id,
        "recipeKey": rcdvlog_core::analysis::STT_RECIPE_KEY,
        "counts": summary,
    }))
}
```

Reuse the existing progress-event emitter the ingest path uses for stderr JSON events. If it is named differently, call that one rather than adding `emit_progress_event`.

Add `Commands::Transcribe` to the catalog-requiring branch near `apps/cli/src/main.rs:361` so it resolves a catalog, matching `Commands::Proxy`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p rcdvlog-cli`
Expected: PASS.
Run: `cargo build --workspace`
Expected: success.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add apps/cli/src/main.rs
git commit -m "feat(cli): transcribe generate, missing, and status commands"
```

---

### Task 13: End-to-end verification against real footage

**Files:**
- Create: `core/rcdvlog-core/tests/stt_end_to_end.rs`
- Modify: `docs/desktop/smoke-test.md`, `AGENTS.md`

**Interfaces:**
- Consumes: the whole stage.
- Produces: an `#[ignore]`d integration test and updated documentation.

**Why this exists:** the design records that the recommended pipeline "was never built or timed end to end" — every round-1 number is a component measurement, and the ~3.5 minute projection for the reference corpus is arithmetic on those, not an observed run. This task produces the first real end-to-end number.

- [ ] **Step 1: Write the failing test**

Create `core/rcdvlog-core/tests/stt_end_to_end.rs`:

```rust
//! End-to-end transcription against real footage.
//!
//! Ignored by default: it needs the downloaded models and a real project with
//! generated proxies. Run explicitly with
//! `RCDVLOG_STT_FIXTURE=<project folder> cargo test -p rcdvlog-core --test
//! stt_end_to_end -- --ignored --nocapture`.

use std::path::PathBuf;
use std::time::Instant;

#[test]
#[ignore = "requires downloaded models and a real project with proxies"]
fn transcribes_a_real_project_and_reports_its_gate_ratios() {
    let Ok(fixture) = std::env::var("RCDVLOG_STT_FIXTURE") else {
        panic!("set RCDVLOG_STT_FIXTURE to a project folder containing catalog.sqlite3");
    };
    let catalog_path = PathBuf::from(&fixture).join("catalog.sqlite3");
    let catalog = rcdvlog_core::Catalog::open(catalog_path).expect("open catalog");
    let projects = catalog.list_projects().expect("list projects");
    let project = projects.first().expect("fixture has at least one project");

    let started = Instant::now();
    let outcomes = catalog
        .transcribe_missing(&project.id)
        .expect("transcribe project");
    let elapsed = started.elapsed();

    let no_speech = outcomes
        .iter()
        .filter(|outcome| {
            outcome.status == rcdvlog_core::analysis::store::TranscriptStatus::NoSpeech
        })
        .count();
    let rejected = outcomes
        .iter()
        .filter(|outcome| {
            outcome.status == rcdvlog_core::analysis::store::TranscriptStatus::Rejected
        })
        .count();
    let windows: u32 = outcomes.iter().map(|outcome| outcome.window_count).sum();

    println!("clips: {}", outcomes.len());
    println!("no_speech: {no_speech}");
    println!("rejected: {rejected}");
    println!("windows decoded: {windows}");
    println!("wall time: {elapsed:?}");

    assert!(!outcomes.is_empty(), "fixture produced no outcomes");
    // Round 1 measured 176 of 273 clips with zero speech on the reference
    // corpus. A run where nothing is gated means the VAD gate is not working,
    // which is the one failure that silently costs both money and correctness.
    assert!(
        no_speech > 0,
        "VAD gated nothing — the correctness gate is not working"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rcdvlog-core --test stt_end_to_end -- --ignored`
Expected: FAIL with the `RCDVLOG_STT_FIXTURE` panic when the variable is unset. That is the correct failure — it proves the harness is wired.

- [ ] **Step 3: Run it for real**

Download both models into `<project folder>/cache/models/`, then:

Run: `RCDVLOG_STT_FIXTURE=~/Movies/VideoProjects/rcdvlog-expo.ykQEF6 cargo test -p rcdvlog-core --test stt_end_to_end -- --ignored --nocapture`
Expected: PASS. Record the printed clip count, no-speech count, rejected count, window count and wall time — this is the first end-to-end measurement of the pipeline and the number round 1 could only project.

- [ ] **Step 4: Document the result**

Add a section to `docs/desktop/smoke-test.md` giving the command above and the observed numbers. Add a `### Transcription` subsection to `AGENTS.md` section 9 recording: the recipe key, the batch task key, that weight is 30-second windows, that audio comes from the proxy, that CPU-only is deliberate and the `metal` feature must stay off, and the observed end-to-end figures. Update the AGENTS.md invariant "AI analysis, transcription, embeddings, and VLM inference are out of the current MVP" to reflect that local transcription now ships.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add core/rcdvlog-core/tests docs AGENTS.md
git commit -m "test(analysis): end-to-end transcription run on real footage"
```

---

## Self-Review

**Spec coverage.** Design §3 maps to Tasks 1–13: audio extraction (6), VAD gate (7, 9, 10), window packing (2), internal auto-detect (10), timestamp translation (3), coverage check (5, 9), reject gate (4, 9), recipe key (2), two-phase batch estimation (11), model provisioning (7), CPU-only (10, 13). Design §7's CLI surface maps to Task 12; the offline/no-key requirement holds because nothing in this plan touches the network. Design §9's testing requirements map to Tasks 3, 5, 9 and 13.

**Deliberately deferred to the VLM plan**, since they belong to that stage: the `analyze` CLI commands, the desktop surface, credentials, `title`/`description` columns, the label origin field, and consolidation. Design §9's synthetic code-switch fixture is covered structurally by Task 5 and Task 9's re-split tests; building the `say`-generated audio fixture belongs with the VLM plan's negative-control fixture work, since both need the same fixture harness.

**Known gap, called out rather than hidden:** `WHISPER_MODEL.blake3` and `VAD_MODEL.blake3` are empty strings, and `ensure_model` skips verification when the digest is empty. Task 13 is where the real files are first downloaded — fill both constants in with the observed digests at that point, and the verification path then becomes live. `ensure_model` deliberately does not download; provisioning a 574 MB file is a separate explicit step, not something a batch starts silently.

---

## Next

The VLM stage is a separate plan (`2026-08-03-vlm-clip-analysis.md`), written after this one lands. It consumes `load_transcript` from Task 8 and the `analysis` module layout from Task 2.
