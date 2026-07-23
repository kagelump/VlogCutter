# AGENTS.md

This file is the operational handoff for agents working on RCD VlogCutter. It applies to the entire repository. Read it before changing code, then read the task-relevant source and the linked documentation. Do not treat the aspirational portions of `PRD.md` as already implemented.

## 1. Product in one paragraph

RCD VlogCutter is a local-first, single-user macOS application for labeling large trip-footage projects. Originals remain immutable and referenced in place on a NAS, removable drive, or local disk. Each project has a laptop-local SQLite catalog plus disposable proxies and thumbnails. The current vertical slice supports project creation/opening, ingest, metadata probing, free-form labels, notes, reversible clip removal, fixed camera-clock offsets, quality-1 thumbnails, H.264 review proxies, and a machine-readable CLI.

The target workload is roughly 100 hours, thousands of mostly 4K/60 clips, and about five cameras (Sony, DJI, iPhone, Insta360, and similar). Optimize data structures and UI choices for that scale even when tests use tiny fixtures.

## 2. Authority and product invariants

Use these sources in this order when they differ:

1. The user's current request and explicit decisions.
2. Section 25 of `PRD.md` for accepted MVP decisions.
3. This file for repository workflow and current implementation facts.
4. `README.md` and the focused documents under `docs/`.
5. The rest of `PRD.md`, which includes planned capabilities that do not exist yet.

Never violate these invariants without explicit user approval:

- Originals are immutable and referenced in place. Never move, rename, rewrite, or delete source media.
- SQLite is the canonical project state. Do not introduce a second canonical JSON store or a permanent background Python service.
- The Rust core owns domain behavior. Desktop and CLI adapters must not implement competing versions of ingest, chronology, proxy, thumbnail, or export logic.
- Generated proxies, thumbnails, and exports live beside the project catalog, not beside originals.
- Review, notes, search, and chronology should remain useful when originals are offline and proxies are available.
- Chronology changes are explicit. Do not silently apply inferred clock or timezone corrections.
- Rough-cut selection belongs to the later edit-planning stage. Do not add inclusion or source-range controls back to the labeling phase.
- The accepted finishing target is the latest Resolve release (Resolve 21 in the product decision), while retaining practical compatibility with the user's installed Resolve 20.
- AI analysis, transcription, embeddings, and VLM inference are out of the current MVP.
- The app must work offline apart from access to mounted source media.

## 3. Current implementation versus PRD target

The implemented slice is intentionally smaller than the launch-critical list in `PRD.md`.

Implemented now:

- One portable SQLite catalog per newly created desktop project.
- Configurable projects root, defaulting to `~/Movies/VideoProjects`.
- Project library discovery and loading an existing catalog.
- Reference-in-place recursive ingest of supported media extensions.
- Sampled fingerprint deduplication within a project.
- ffprobe technical/timestamp metadata and ExifTool camera make/model lookup.
- Label queue, playback, free-form labels, notes, reversible removal, and keyboard shortcuts.
- Fixed camera offset events from a filename boundary onward.
- Quality-1 thumbnails and one-worker 720p H.264/AAC proxies.
- Weighted batch progress and ETA with project-persisted rate history.
- Relative-order “story ribbon” with camera lanes, horizontal scrolling, and zoom.
- A non-interactive JSON CLI for catalog, clip annotation, chronology, and media operations.
- Immutable typed project operations for asset membership, notes, labels, removal, rotation, and fixed clock events, with deterministic materialized views and conflict diagnostics.
- Read-only catalog/project/clip/proxy diagnostics, including the desktop's persisted last-selected catalog, project, and asset breadcrumb.

Important gaps; do not accidentally describe these as complete:

- No ingest preview or capture-sequence split/merge workflow.
- No durable resumable job queue, cancellation, leases, or restart recovery.
- No relink workflow, mounted-volume inventory, project deletion, or cache cleanup UI.
- No revision tokens or optimistic-concurrency model for desktop/CLI writes.
- Search is currently simple filename/camera/label/note filtering, not SQLite FTS.
- No key moments, chapters, events, scripts, edit-plan revisions, rough-cut selection, timeline export, or preview render.
- No trip-phase/location/timezone/story-beat persistence or editor. The TypeScript `Asset` context fields and browser mock only make the story-ribbon display contract visible; the Rust model and SQLite schema do not store them yet.
- No expanded chronology calibration view, clip exceptions, contradiction reporting, or confidence/evidence explanation.
- No automated React component/unit test runner and no automated native Tauri UI suite.
- No macOS signing/notarization or clean-machine packaging acceptance.
- Native media playback, filesystem permission prompts, sleep/wake, and NAS-scale behavior remain manual acceptance checks. Resolve import returns as a later edit-planning acceptance check.

Before implementing persisted trip context, resolve the domain question of whether story beats are independent overlapping ranges or must be nested under physical phases. The current recommendation is independent ranges because narrative beats may cross a location or timezone transition.

## 4. Repository map

```text
.
├── AGENTS.md                         # This operational handoff
├── PRD.md                            # Product scope, architecture, milestones, decisions
├── README.md                         # Short setup and usage guide
├── Cargo.toml                        # Rust workspace
├── package.json                      # Root pnpm command aliases
├── apps/
│   ├── cli/
│   │   ├── src/main.rs               # clap CLI, JSON envelopes, error mapping
│   │   └── tests/cli_workflow.rs     # Process-level CLI/media integration tests
│   └── desktop/
│       ├── src/
│       │   ├── App.tsx               # Main React UI and interaction state
│       │   ├── api.ts                # Sole React↔Tauri boundary plus browser mocks
│       │   ├── types.ts              # TypeScript DTO mirrors
│       │   └── styles.css            # Entire current visual system
│       ├── src-tauri/
│       │   ├── src/lib.rs            # Tauri state, commands, settings, project library
│       │   ├── src/main.rs            # Thin native entry point
│       │   ├── tauri.conf.json       # Window, bundle, and asset-protocol config
│       │   └── capabilities/         # Native capability permissions
│       ├── package.json              # Vite/TypeScript/Tauri scripts
│       └── vite.config.ts            # Fixed dev port 1420
├── core/
│   ├── rcdvlog-core/src/lib.rs       # Canonical catalog, media, annotation, chronology logic
│   ├── rcdvlog-core/src/operations.rs # Typed operations, HLC, materialization, conflicts
│   └── batch-estimator/src/lib.rs    # Domain-independent weighted ETA library
└── docs/
    ├── agent-cli.md                  # Full CLI and JSON contract
    ├── batch-estimation.md           # Estimator semantics and persistence
    └── integration-testing.md        # Automated versus manual acceptance boundaries
```

Generated directories such as `target/`, `node_modules/`, `apps/desktop/dist/`, TypeScript build info, and Tauri schema output are not source.

## 5. Architecture and data flow

```text
React/TypeScript
    │ typed calls and progress listeners (`apps/desktop/src/api.ts`)
    ▼
Tauri commands (`apps/desktop/src-tauri/src/lib.rs`)
    │ thin validation/state/async boundary
    ▼
Rust core (`core/rcdvlog-core`)
    ├── SQLite catalog in WAL mode
    ├── managed ffmpeg / ffprobe / exiftool child processes
    ├── project-local cache files
    └── weighted ETA library (`core/batch-estimator`)

CLI (`apps/cli`) ─────────────────────┘
```

Rules for this boundary:

- React code calls the exported `api` object. Do not scatter direct `invoke(...)` calls through components.
- Tauri commands should be thin wrappers over `Catalog` methods. CPU/media work must use `spawn_blocking` so it does not block the async runtime.
- The CLI must call the same `Catalog` methods as Tauri.
- Shared DTOs use Serde `camelCase`. Clip labels are free-form strings and `removed` is a reversible visibility flag.
- Register every new Tauri command in `tauri::generate_handler![...]` and add a matching method/type in `api.ts`/`types.ts`.
- Expose agent-relevant domain operations through the CLI as stable JSON, not human-formatted text.

## 6. Toolchain and prerequisites

Minimum documented environment:

- Apple Silicon Mac, macOS 13+
- Xcode command-line tools
- Rust 1.85+ and Cargo
- Node.js 22+
- pnpm 10
- FFmpeg 7+ and ffprobe on `PATH`
- ExifTool on `PATH`

The development environment was last verified on 2026-07-19 with Rust/Cargo 1.97.1, Node 24.11, pnpm 10.17, FFmpeg/ffprobe 7.1.3, and ExifTool 13.36.

Check the environment with:

```bash
rustc --version
cargo --version
node --version
pnpm --version
ffmpeg -version | head -1
ffprobe -version | head -1
exiftool -ver
```

If Rust was just installed and `cargo` is not found, start a fresh login shell or load the standard Cargo environment before assuming the toolchain is missing. The app itself calls `prerequisite_status` at startup and reports missing media tools.

Install JavaScript dependencies from the repository root:

```bash
pnpm install
```

Rust dependencies are resolved by Cargo from the checked-in `Cargo.lock`.

## 7. Running the app

Run the real Tauri desktop application:

```bash
pnpm dev
```

Open a specific existing catalog at startup:

```bash
RCDVLOG_CATALOG=/absolute/path/to/catalog.sqlite3 pnpm dev
```

The Vite-only browser preview is useful for fast layout and interaction work:

```bash
pnpm --dir apps/desktop dev --host 127.0.0.1
```

It runs at `http://127.0.0.1:1420/` and uses in-memory mock projects/assets from `apps/desktop/src/api.ts`. It cannot validate native dialogs, Tauri commands/events, asset-protocol media URLs, FFmpeg child processes, persistence, or filesystem permissions. Never treat a browser-only pass as an end-to-end media test.

Build outputs:

```bash
pnpm --dir apps/desktop build       # TypeScript + Vite frontend
pnpm build:cli                      # target/debug/rcdvlog
pnpm build                          # Tauri application bundle
```

`pnpm build` may require the native macOS/Tauri packaging environment and takes longer than the frontend build.

## 8. Project discovery, catalogs, and storage

New desktop projects default to one folder per project:

```text
~/Movies/VideoProjects/<safe project name>/
├── catalog.sqlite3
├── catalog.sqlite3-wal             # while active; SQLite-managed
├── catalog.sqlite3-shm             # while active; SQLite-managed
└── cache/
│   ├── proxies/<project-id>/<asset-id>.mp4
│   └── thumbnails/<asset-id>/q1.v2.jpg
```

The projects parent folder is configurable in global settings. The desktop scans immediate child directories for `catalog.sqlite3`; invalid catalogs are skipped. The settings file is stored in the macOS application-data directory for `com.rcd.vlogcutter` (normally under `~/Library/Application Support/`). A legacy application-support catalog is still used when the app/CLI is started without an explicit per-project catalog.

Catalog selection precedence for the CLI:

1. `--catalog /absolute/path/catalog.sqlite3`
2. `RCDVLOG_CATALOG`
3. Legacy application-support catalog

Always pass `--catalog` in agent automation, tests, and temporary workflows. A desktop project folder can otherwise be confused with the legacy catalog.

SQLite connections enable foreign keys, WAL journal mode, and a five-second busy timeout. Read operations can continue during an uncommitted writer. Project mutations now append typed operations with materialized revisions; transport-level optimistic concurrency and sync cursors remain deferred to Tasks 8–11.

Project catalogs use an explicit `schema_migrations` history and are currently at version 5. Migration definitions have immutable names/source identities and each migration commits atomically. When changing the schema:

- Preserve existing catalogs; never solve migration issues by deleting the database.
- Add a forward migration and an upgrade test using an older schema fixture.
- Back up or otherwise provide recovery before any destructive rewrite.
- Keep Rust row order, Serde DTOs, TypeScript mirrors, CLI JSON tests, and queries synchronized.
- Remember that `Catalog::open_existing` validates that `projects` and `assets` exist before opening read/write and migrating.

## 9. Core data and media behavior

### Assets and patches

`Asset` is defined canonically in `core/rcdvlog-core/src/lib.rs` and mirrored in `apps/desktop/src/types.ts`. Availability is computed on read:

- `originalAvailable`: original path currently exists.
- `proxyAvailable`: stored proxy path currently exists.
- `available`: original or proxy is available.
- Thumbnail path/quality are returned only when the stored file exists.

The Rust/Tauri `AssetPatch` is a full annotation replacement: notes, labels, removed, and rotation are all required. React's `patchFor(asset)` deliberately sends the full current state. The CLI offers patch-like optional flags by fetching the current asset, merging changes, and then sending the full core patch. Do not change one side without preserving this behavior.

### Ingest and identity

Ingest recursively scans without following symlinks and accepts these extensions case-insensitively:

```text
mp4 mov m4v mts m2ts avi
```

Identity uses a sampled BLAKE3 fingerprint based on file size plus up to 1 MiB from the beginning, middle, and end. The unique database constraint is `(project_id, fingerprint)`. Re-ingest is idempotent within a project. Originals stay at their existing paths.

ffprobe supplies duration, frame rate, dimensions, codec, tags, and timestamp candidates. ExifTool currently supplements camera make/model; absence or failure should degrade to other metadata rather than prevent every ingest.

### Chronology

Assets are ordered deterministically by:

```text
best_time IS NULL, best_time, filename, id
```

Raw camera timestamp text is preserved separately. `best_time` is normalized RFC 3339 UTC when parseable. A clock event applies a fixed offset to one camera from a lexical filename boundary onward. Recalculation chooses the latest applicable boundary and never mutates source metadata.

The current compact story ribbon spaces clips by ordinal position, not duration or elapsed wall-clock distance. This is intentional: relative ordering and context are more important than absolute time span. Default column width is 68 px, `[`/`]` zoom, and `C` centers the selected clip. Do not revert it to a fit-all duration plot.

### Proxies

Proxy generation currently uses one local worker and FFmpeg settings equivalent to:

- Override the input display rotation to 0 and disable autorotation, preserving raw encoded orientation without copying the camera display matrix.
- Scale down to at most 1280 px wide while preserving aspect ratio.
- H.264 `libx264`, `veryfast`, CRF 23, `yuv420p`.
- AAC 128 kb/s and `+faststart`.

Only the original is accepted as proxy input. If the original is offline, proxy generation fails explicitly. Version-3 proxies are written to a temporary file, verified with ffprobe to have 0° display rotation, and then atomically committed asset-by-asset. Version-2 proxies are obsolete because they may retain the source display matrix; a later “missing” run regenerates them and skips only valid current-version paths that still exist.

### Thumbnails

Only quality 1 is implemented. It is a JPEG at 20% of clip duration, at most 480 px wide, generated first to `q1.v2.tmp.jpg` and atomically renamed to `q1.v2.jpg`. Input preference is original, then available proxy. Qualities 2–10 are reserved and must continue to fail clearly until intentionally implemented.

### Batch estimation

All long batch work should use `core/batch-estimator`, not an ad hoc ETA. Callers provide work weights, explicit worker count, optional persisted `RateHistory`, and successful per-worker elapsed observations. The estimator balances remaining weights across workers and reports the slowest shard. Failures advance item completion but do not contaminate throughput.

Current task keys are versioned:

```text
proxy.h264-720p.orientation-v3
thumbnail.q1.orientation-v2
```

Change the task key when settings change enough that old throughput is no longer comparable. Proxy and thumbnail weights are source bytes and both currently use one worker. A future cloud/VLM task must pass its real shard concurrency.

### Deferred edit planning and export

The labeling phase intentionally has no rough-cut inclusion, source ranges, or OTIO export. Those concepts return in a later edit-planning stage after clips are labeled. Keep any future edit-plan state separate from labels and reversible removal; removal only controls visibility in the labeling workflow.

## 10. Desktop UI conventions

The UI is intentionally keyboard-first and dense, using one main `App.tsx` today. Keep domain logic out of it even if component extraction is useful.

### API and state

- `apps/desktop/src/api.ts` selects real Tauri calls when `__TAURI_INTERNALS__` exists and mock behavior otherwise.
- Optimistic asset updates call the core and replace the returned canonical `Asset`; failures roll back and surface an error.
- Progress events are `proxy-progress` and `thumbnail-progress`. Each carries an optional completed asset plus a batch estimate. Merge a completed asset immediately so thumbnails/proxies appear as they finish.
- Media playback prefers an available proxy, otherwise an available original. Thumbnails are also used as the video's poster frame.
- Playback controls are custom stage-level UI so they remain horizontal while the media element rotates. Do not re-enable native `<video controls>` unless rotation is moved off the video element; native controls inherit its CSS transform.
- The React `assets` array is the in-memory metadata cache. Legacy orientation probing uses a two-worker, deduplicated priority queue for current, next, next-next, and previous clips; successful probes persist to SQLite and replace the cached asset.
- Playback uses a bounded three-slot sliding window (previous/current/next). The next available representation is preloaded automatically (proxy preferred, otherwise one original), the previous slot stays mounted but requests metadata only, and clips outside the window are unmounted. Preserve these bounds unless memory, file-handle, and NAS-read measurements justify a wider cache.
- Global autoplay settings are `never`, `proxyAvailable` (default), and `always`.

### Keyboard contract

Preserve and update both visible hints and the `?` keyboard map when shortcuts change. Current important behavior:

- `J`/`K` or arrow keys: next/previous clip.
- Space: play/pause.
- `L`: focus the free-form label field; Enter adds the label.
- `R`: remove the selected clip from the labeling queue, or restore it in the Removed view.
- `N`: focus notes. Consume the original `n` key event before focusing so it is not inserted.
- Notes: Enter saves; Command+Enter inserts a newline; Shift+Enter saves and advances; Escape discards the current editing session and removes focus. After advancing, notes must not remain focused.
- `/`: focus queue search.
- `P`: selected proxy; Shift+`P`: missing proxies; `T`: missing thumbnails.
- `M`: add media.
- `[`/`]`: story-ribbon zoom; `C`: center selected clip.
- Command+`N`: new project; Command+`O`: open catalog; Command+`,`: global settings; `?`: keyboard map.

Global shortcuts must return early for text inputs, textareas, and selects, except modal-level Escape handling. Any shortcut that moves focus must call `preventDefault()` before focusing the destination.

### Story-ribbon context

The ribbon already accepts optional TypeScript fields `phase`, `location`, `timezone`, and `storyBeat` and groups contiguous equal contexts. These fields are populated only in the browser demo. When persisted context is added, prefer a core-derived per-asset display projection from explicit time/range records rather than making the optional UI fields an independent source of truth.

### CSS and layout

The app has a minimum 980×680 window and a three-column workbench. Review queue and inspector scroll independently; the player and story ribbon occupy the center. Preserve visible focus styles, semantic labels, `aria-keyshortcuts`, and reduced-clutter dark editing-bench aesthetic. Test at the configured 1440×900 default and the 980×680 minimum.

The Tauri asset protocol currently permits app-data cache paths, `$HOME/**`, `/Volumes/**`, and temporary RCD VlogCutter cache fixtures. If media paths outside these scopes are introduced, update permissions deliberately and validate on macOS; do not disable security globally as a shortcut.

## 11. CLI contract for agents

Build with:

```bash
pnpm build:cli
```

The binary is `target/debug/rcdvlog`. See `docs/agent-cli.md` for every command.

Contract rules:

- Success: exactly one JSON envelope on stdout and exit 0.
- Failure: stdout stays empty; exactly one JSON error envelope goes to stderr with nonzero exit.
- Long ingest progress may write newline-delimited JSON events to stderr before the final stdout result.
- Stable command names and error codes are public automation contracts.
- Current error codes: `invalid_arguments`, `invalid_input`, `not_found`, `media_offline`, `media_tool_failed`, and `internal_error`.
- `doctor` must not create or mutate the selected catalog.
- Read commands should remain usable while another connection has an uncommitted WAL write.

Typical explicit-catalog workflow:

```bash
target/debug/rcdvlog --catalog /absolute/project/catalog.sqlite3 doctor
target/debug/rcdvlog --catalog /absolute/project/catalog.sqlite3 project list
target/debug/rcdvlog --catalog /absolute/project/catalog.sqlite3 project status --project <project-id>
target/debug/rcdvlog --catalog /absolute/project/catalog.sqlite3 clips list --project <project-id> --limit 20
```

Screenshot-driven debugging is a normal agent workflow for this repository. The user may provide only a screenshot and expect the agent to recover the relevant app state. Begin with `desktop last-opened`, then use `catalog inspect`, `project diagnose`, `clips diagnose`, and `proxy status` as appropriate. The desktop breadcrumb is the last persisted selection, so compare its timestamp and stable IDs with the screenshot rather than claiming it is live IPC. Diagnostic commands must remain strictly read-only: do not run migrations, rewrite metadata, or generate cache files while observing. Keep stored catalog state, live filesystem/media observations, derived render behavior, warnings, and recommended explicit mutations separate in JSON.

When adding a selectable UI entity or a failure-prone representation, extend this diagnostic path at the same time: persist its stable selected ID, add a focused machine-readable diagnosis, include representation/version identity and external availability, and explain important derived decisions. Mutations must be separate, deliberately named commands. This convention is what makes the CLI reusable for future screenshot-led issues instead of accumulating one-off database queries.

Never parse human prose from the CLI. Add/extend JSON contract tests whenever commands, fields, errors, or progress events change.

## 12. Testing and verification

Run the narrowest useful tests while iterating, then the full gate before handoff.

| Change area | Fast check | Required broader check |
| --- | --- | --- |
| React/TypeScript/CSS | `pnpm --dir apps/desktop check` | `pnpm --dir apps/desktop build` plus interaction/visual check |
| Rust core | `cargo test -p rcdvlog-core` | `cargo test --workspace` |
| ETA library | `cargo test -p batch-estimator` | `cargo test --workspace` |
| CLI parsing/JSON | `cargo test -p rcdvlog-cli` | `pnpm test:integration` |
| Tauri commands/settings | `cargo test -p rcdvlog-desktop` | real `pnpm dev` smoke test when native behavior changed |
| Formatting/lints | `cargo fmt --all -- --check` | `cargo clippy --workspace --all-targets -- -D warnings` |
| Full repository | — | `pnpm check` |

`pnpm check` runs TypeScript checking followed by all Rust workspace tests. The process-level integration suite creates a real one-second FFmpeg fixture and validates CLI persistence, WAL reads, ingest, metadata, deduplication, labels/notes/removal, clock correction, thumbnails/proxies, and offline-original review. Its media portion skips when FFmpeg or ffprobe is missing.

Do not replace these real process/media boundaries with mocks. Conversely, do not pretend headless tests validate:

- DaVinci Resolve import.
- macOS dialogs and filesystem permission prompts.
- Tauri asset-protocol playback.
- Sleep/wake or source-volume disconnect/reconnect.
- Expo-scale NAS performance.

For frontend interaction changes, use the Vite browser preview for focus, keyboard, scrolling, progress, and layout behavior, then use Tauri when the change depends on native APIs. Inspect the actual rendered UI; a passing TypeScript build does not catch focus propagation, overflow, or unreadable density.

## 13. Real-project acceptance fixture

The known large source fixture is:

```text
/Volumes/data/VideoProjects/2025-08 Expo
```

It is source media, not a disposable test directory. Before accessing it:

- Confirm the volume is mounted and macOS permission has been granted.
- Enumerate read-only first; permission prompts may require the user to click Allow and retry.
- Never write generated files into the source tree.
- Use a project catalog under the configured laptop-local projects root.
- Start with a single source/small subset for smoke testing before any all-media batch.
- Report exact scan/import/failure counts and preserve partial successful work.

Scale acceptance should cover thousands of assets, offline behavior, progress responsiveness, estimated time remaining, horizontal story-ribbon navigation, and restart/interruption behavior. It is periodic manual acceptance, not part of every test run.

## 14. Change recipes

### Add a new domain operation

1. Add or change canonical types and validation in `rcdvlog-core`.
2. Add core unit tests, including invalid input and missing/offline state.
3. Add a thin Tauri command and register it.
4. Mirror DTOs in `types.ts` and expose the call only through `api.ts`.
5. Add UI behavior, accessible labels, keyboard access, error state, and progress if relevant.
6. Add a CLI command when agents need the operation; preserve JSON envelopes and stable error codes.
7. Add process-level integration coverage for cross-boundary behavior.
8. Update the relevant document under `docs/` and this file if architecture or workflow changed.

### Add a long-running batch task

1. Choose a meaningful work unit and versioned task key.
2. Declare the actual worker count; never assume all jobs are single-worker.
3. Use `BatchEstimator` and persisted per-project rate history.
4. Emit initial progress, each completed asset/result, failures, worker count, and ETA.
5. Commit successes incrementally so interruption does not discard completed work.
6. Do not let failed observations poison speed history.
7. Update assets in the UI as each item completes.
8. Test heterogeneous task sizes, worker sharding, no-history estimation, and failures.

### Add or change project settings

1. Update Rust `DesktopSettings` with a Serde default so older settings files remain valid.
2. Update the TypeScript mirror and browser mock storage.
3. Persist atomically where practical and test a missing/older/corrupt settings file.
4. Expose the control in global settings, not per-project UI, unless the setting truly belongs in the catalog.

### Change media generation settings

1. Treat outputs as a new representation version.
2. Change cache/task keys so stale files and throughput history are not silently reused.
3. Update docs, tool checks, tests, and offline fallback behavior.
4. Test representative Sony/DJI/iPhone/Insta360 H.264/H.265 media, not only synthetic MPEG-4 fixtures.

## 15. Safety and working-tree discipline

- Inspect `git status` before editing. The repository may contain uncommitted or untracked user work; preserve it.
- Keep edits scoped. Do not reformat large unrelated files, especially the current large single-file Rust core and React app.
- Never delete catalogs, source media, project folders, proxies, thumbnails, or exports unless the user explicitly requests the exact destructive action.
- Tests must use `tempfile`/temporary directories. Never point destructive test setup at `~/Movies/VideoProjects`, `/Volumes`, or the repository root.
- Do not edit SQLite by hand to achieve product behavior. Add a domain operation and migration.
- Do not use the browser mock as evidence that persistence or media processing works.
- Avoid adding always-running services, network dependencies, analytics, or credential storage.
- Keep child-process arguments structured with `Command`; do not build shell strings from media paths.
- Preserve paths with spaces and non-ASCII filenames.
- Surface per-file failures without aborting an otherwise recoverable batch.

## 16. Definition of done

A change is ready to hand off when:

- It respects the source-of-truth and original-media invariants.
- Canonical behavior is in Rust core and both adapters remain consistent.
- DTO casing and full-patch semantics match across Rust, Tauri, CLI, and TypeScript.
- Relevant unit and integration tests cover success, invalid input, offline/missing files, and persistence where applicable.
- Keyboard and focus behavior, visible progress, scrolling, and minimum-window layout were exercised for UI changes.
- `cargo fmt --all -- --check`, relevant Clippy/tests, and at minimum the appropriate TypeScript build pass.
- Documentation and CLI contracts were updated if user-visible behavior changed.
- Known manual acceptance that was not run is stated explicitly.
- No generated output or real source media was accidentally added or modified.
