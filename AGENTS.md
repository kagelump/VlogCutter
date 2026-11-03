# AGENTS.md

This file is the operational handoff for agents working on RCD VlogCutter. It applies to the entire repository. Read it before changing code, then read the task-relevant source and the linked documentation. Do not treat the aspirational portions of `PRD.md` as already implemented.

This repository-root `AGENTS.md` describes the *software repository*. It is not the same artifact as the generated `AGENTS.md` that will one day live inside each user's video project root (PRD §6.3, issue #48). That one describes a footage project to a coding agent; this one describes source code to a contributor. Do not conflate them.

## 1. Product in one paragraph

RCD VlogCutter is a local-first, single-user footage labeling system. The macOS desktop references immutable originals in place, except that its explicit card-import workflow may copy from a read-only removable card into a named create-only archive root, and keeps one laptop-local SQLite catalog per project. The optional private Ubuntu companion server keeps its own local-SSD control catalog and project replicas, indexes read-only NAS roots, runs durable media jobs, serves Chrome review/admin, synchronizes immutable project operations, and creates verified backups. The Mac remains useful offline and neither node shares live SQLite files.

The target workload is roughly 100 hours, thousands of mostly 4K/60 clips, and about five cameras (Sony, DJI, iPhone, Insta360, and similar). Optimize data structures and UI choices for that scale even when tests use tiny fixtures.

## 2. Authority and product invariants

Use these sources in this order when they differ:

1. The user's current request and explicit decisions.
2. Section 25 of `PRD.md` for accepted MVP decisions.
3. This file for repository workflow and current implementation facts.
4. `README.md` and the focused documents under `docs/`.
5. The rest of `PRD.md`, which includes planned capabilities that do not exist yet.

Never violate these invariants without explicit user approval:

- Originals are immutable. Non-removable media is referenced in place. The
  explicit removable-card workflow may copy source bytes into a named,
  create-only archive root; it never moves, renames, rewrites, or deletes card
  media, and never overwrites an archived original. See
  `docs/tasks/import/ARCHITECTURE.md` §2.1.
- SQLite is canonical. Portable project catalogs own project operations; the server's local-SSD control catalog owns server jobs/library/auth and its local replicas. Do not introduce a competing JSON store, share SQLite/WAL across nodes, or add a permanent Python service.
- The accepted agent architecture (PRD §25.1 item 18) is a project-local
  workspace layered over the revision-safe CLI. Workspace files are the agent
  discovery and context layer; the CLI is the safe machine-control layer; SQLite
  stays canonical. When the workspace lands, do not let a generated projection
  become a second write path: hand-editing `PROJECT.md` or `edit-plan.json` must
  not mutate catalog state. A custom embedded AI harness and an MCP server are
  both out of scope for the MVP.
- The owning Rust crate controls domain behavior. Desktop, web, HTTP, Tauri, and CLI adapters must not implement competing ingest, sync, job, storage, chronology, or representation logic.
- Generated outputs live in project-local caches or the configured server derived root, never beside originals.
- Review, notes, search, and chronology should remain useful when originals are offline and proxies are available.
- Chronology changes are explicit. Do not silently apply inferred clock or timezone corrections.
- Rough-cut selection belongs to the later edit-planning stage. Do not add inclusion or source-range controls back to the labeling phase.
- The accepted finishing target is the latest Resolve release (Resolve 21 in the product decision), while retaining practical compatibility with the user's installed Resolve 20.
- Local transcription and bounded cloud clip analysis ship (see "Transcription"
  and "Clip analysis" in section 9). Local VLM inference, embeddings, and
  semantic search remain out of scope. Analysis output is a suggestion layer:
  it never overwrites human notes or human-authored labels, and rough-cut
  selection stays in the edit-planning stage.
- The desktop must work offline apart from access to its mounted source media. The server is optional and never required to commit local annotations.
- Server original roots are immutable/read-only. The planned travel archive is a separate future create-only writable logical root, not an exception for existing scan roots.
- The raw server backend remains loopback-only behind trusted LAN Caddy or Tailscale Serve HTTPS. Never enable Funnel or put credentials in URLs/argv.

## 3. Current implementation versus PRD target

The implemented slice is intentionally smaller than the launch-critical list in `PRD.md`.

**Convention (keeps this file an index):** a feature PR adds *one index line plus a link* here, not a paragraph. Schema numbers, contracts, measurements, and acceptance detail belong in the feature's focused doc under `docs/`; if none fits, add it to [docs/implementation-status.md](docs/implementation-status.md), which holds the full text of everything below. `pnpm check:docs` fails if this file grows past its size limit.

Implemented now (detail in [docs/implementation-status.md](docs/implementation-status.md) unless a doc is named):

- **Projects and ingest**: one portable SQLite catalog per desktop project, configurable projects root, library discovery, reference-in-place ingest, sampled-fingerprint dedup with repair of offline assets, verified node-local media locations, ffprobe/ExifTool metadata. Smart ingest dialog (preflight classification, archive roots, atomic card copy with optional verification, camera choice, cancel/resume, ledger) and macOS removable-card detection: [docs/tasks/import/ARCHITECTURE.md](docs/tasks/import/ARCHITECTURE.md).
- **Labeling**: queue, playback, labels, notes, reversible removal, label typeahead, transcript-aware search.
- **Chronology**: capture sequences, clock epochs, trip phases, deterministic solver, diagnostics, story ribbon, Chronology desk, camera-lane merge; `.LRF` sidecars ([docs/camera-proxy-sidecars.md](docs/camera-proxy-sidecars.md)). See [docs/tasks/desktop/CHRONOLOGY_MODEL.md](docs/tasks/desktop/CHRONOLOGY_MODEL.md) and [docs/edit-plan-chronology.md](docs/edit-plan-chronology.md).
- **Representations**: Q1 thumbnails and 720p proxies ([docs/proxy-generation.md](docs/proxy-generation.md)); orientation ([docs/orientation-review.md](docs/orientation-review.md)); weighted ETA ([docs/batch-estimation.md](docs/batch-estimation.md)).
- **Analysis**: local transcription, cloud proxy-only clip analysis, moments, `prepare` CLI, desktop analysis surface, accounting/preflight/native video: [docs/analysis-preflight.md](docs/analysis-preflight.md), [docs/analysis-accounting.md](docs/analysis-accounting.md), [docs/native-video-review.md](docs/native-video-review.md); full recipes in [docs/core-media-reference.md](docs/core-media-reference.md).
- **Edit planning and review** (schemas 54-66): preview artifacts, inspection, context review, comparisons, selected-audio integrity, chapter selection, feedback, source ranges, coverage, delivery, readiness/doctor, OTIO writing. Start at [docs/rough-cut-readiness.md](docs/rough-cut-readiness.md) and [docs/agent-cli.md](docs/agent-cli.md); each capability has its own `docs/edit-plan-*.md` (e.g. [feedback](docs/edit-plan-feedback.md), [source ranges](docs/edit-plan-source-ranges.md), [preview artifacts](docs/edit-plan-preview-artifacts.md)) and [docs/selected-audio-integrity.md](docs/selected-audio-integrity.md).
- **TV review** (companion server schemas 13-16): target export/import, Chrome voice-review recording, review-session ASR, discussions, acoustic markers, sync, soundtrack matching. Start at [docs/tv-review-handoff.md](docs/tv-review-handoff.md); siblings are the `docs/tv-review-*.md`, [docs/tv-voice-review-recording.md](docs/tv-voice-review-recording.md), and [docs/tv-soundtrack-matching.md](docs/tv-soundtrack-matching.md).
- **Desktop shell**: 13" three-row layout (topbar, workbench, story ribbon) with layout invariants enforced by tests, evidence shelf, Focus mode, `projectSession.ts` read owner, `assetMutations.ts` write owner, loopback media origin: [docs/superpowers/specs/2026-08-04-desktop-review-layout-design.md](docs/superpowers/specs/2026-08-04-desktop-review-layout-design.md), [docs/tasks/desktop/MEDIA_DELIVERY.md](docs/tasks/desktop/MEDIA_DELIVERY.md).
- **CLI and operations**: one JSON CLI (schema 66), typed immutable project operations, read-only diagnostics: [docs/agent-cli.md](docs/agent-cli.md).
- **Companion server** (control schema 16): control catalog, replicas, storage, jobs, sync 1.0, auth, Chrome review/admin, backups: [docs/server/README.md](docs/server/README.md).
- **Publishing**: YouTube upload-only OAuth and resumable upload: [docs/youtube-upload.md](docs/youtube-upload.md).

Important gaps; do not describe these as complete (detail in [docs/implementation-status.md](docs/implementation-status.md)):

- **Import/volumes**: preflight capture-sequence decisions are read-only proposals (the Chronology desk applies split/merge/reassign; "same card" is the reversible `captureSequenceSplitDeclined` operation). Project deletion and cache cleanup ship (#16); there is no mounted-volume inventory UI. Relink exists. Physical card/volume acceptance is unrun: [docs/tasks/import/EPIC.md](docs/tasks/import/EPIC.md).
- **Revisions**: `update_asset` is a full-patch write guarded by `expectedRevision`; it cannot tell whose value won when two writers edit the same field, and cross-node operation sync is unchanged.
- **Search** is simple filename/camera/label/note/transcript filtering, not FTS; only flattened transcript text is indexed.
- **Edit planning**: moments and key moments are sparse suggestions and certify no cut range. No separate chapter/event/script entities and no script revisions. The desktop edit-plan surface still lacks reordering, trimming, removal, preview playback, export history, and overwrite confirmation.
- **No materialized project-local agent workspace** (issue #48, PRD Milestone 5): the CLI only binds/checks `.vlogcutter/cli-binding.json`; it never generates `AGENTS.md`, `PROJECT.md`, `script.md`, `project.json`, or `edit-plan.json`.
- **Trip context**: phases and timezones are persisted; story beats are display-only browser-mock fields, not persisted. Before persisting them, decide whether beats are independent ranges (current recommendation) or nested under phases. Clip-exception and event-order controls are domain/API only, not desk tools.
- **Analysis suggestions** have no per-suggestion accept/reject workflow; a forced re-run overwrites AI titles and descriptions.
- **Tests/packaging**: no native Tauri UI suite; no macOS signing/notarization or clean-machine packaging acceptance.
- **Manual acceptance not run**: native media playback and the loopback origin's three webview criteria (un-played first frame, non-zero `drawImage` alpha before `play()`, seek to an unbuffered region), permission prompts, sleep/wake, NAS scale, Resolve import, Ubuntu/Compose, NFS/SMB outage, Caddy/Tailscale HTTPS, native pairing, target resource/thermal ([docs/tasks/server/ACCEPTANCE_TASK_19.md](docs/tasks/server/ACCEPTANCE_TASK_19.md)).
- No hardware acceptance is implied by `tv_soundtrack` candidates, fake native-video protocol tests, or TV review features.

## 4. Repository map

- `apps/cli` (clap JSON CLI, `src/main.rs`), `apps/desktop` (React in `src/`, Tauri in `src-tauri/`), `apps/server` (Axum runtime and admin CLI), `apps/web` (Chrome review/admin).
- `core/rcdvlog-core` (canonical catalog, media, annotation, chronology; `src/lib.rs`, `src/operations.rs`), `core/rcdvlog-server`, `core/rcdvlog-sync`, `core/batch-estimator`.
- `packages/project-client` (OpenAPI snapshot, typed browser client), `packages/review-ui`, `deploy/server/`, `scripts/`, `docs/`, `PRD.md`, `README.md`.

Annotated tree: [docs/repository-map.md](docs/repository-map.md). Generated directories (`target/`, `node_modules/`, `apps/desktop/dist/`, TypeScript build info, Tauri schema output) are not source.

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

Desktop sync manager ── authenticated HTTPS operations ──▶ Axum (`apps/server`)
                                                           │
Chrome (`apps/web`) ── generated typed client ─────────────┤
                                                           ▼
                                                 Server Rust domains
                                                 ├── local SQLite catalogs
                                                 ├── read-only originals
                                                 ├── derived/backup roots
                                                 └── durable workers
```

Rules for this boundary:

- React code calls the exported `api` object. Do not scatter direct `invoke(...)` calls through components.
- Tauri commands should be thin wrappers over `Catalog` methods. CPU/media work must use `spawn_blocking` so it does not block the async runtime.
- The CLI must call the same `Catalog` methods as Tauri.
- Shared DTOs use Serde `camelCase`. Clip labels are free-form strings and `removed` is a reversible visibility flag.
- Register every new Tauri command in `tauri::generate_handler![...]` and add a matching method/type in `api.ts`/`types.ts`.
- Expose agent-relevant domain operations through the CLI as stable JSON, not human-formatted text.
- Browser calls go through `packages/project-client`; the Rust-owned OpenAPI document and checked-in snapshot must remain in sync.
- Server HTTP handlers are thin auth/validation/transport adapters over `rcdvlog-server`/`rcdvlog-core`/`rcdvlog-sync` domains. They never accept arbitrary filesystem paths outside configured admin boundaries.
- WebSocket events are hints; durable state and bounded HTTP reconciliation/polling remain authoritative.

## 6. Toolchain and prerequisites

Apple Silicon Mac, macOS 13+; Xcode command-line tools; Rust 1.85+; Node.js 22+ and pnpm 10; FFmpeg 7+/ffprobe and ExifTool on `PATH`. Install JS dependencies with `pnpm install` from the root. The app reports missing media tools at startup (`prerequisite_status`).

The pre-PR gate and CI pin Rust 1.97.1 (with `rustfmt` and `clippy`), Node.js 24.11.0, and pnpm 10.17.0. Never let a missing pinned tool silently fall back to another Rust version or formatter; `pnpm gate` verifies the pin. If `cargo` is not found, load `~/.cargo/env` or an explicitly configured `CARGO_HOME`/`RUSTUP_HOME` before assuming it is missing. For an agent development binding, build with `cargo +1.97.1 build --locked -p rcdvlog-cli` and pass the resulting absolute binary path to `workspace refresh`; `workspace check` never resolves a replacement executable. Install commands, version-check commands, and the verified-toolchain record: [docs/dev-environment.md](docs/dev-environment.md).

## 7. Running the app

`pnpm dev` runs the real Tauri app (`RCDVLOG_CATALOG=/abs/catalog.sqlite3 pnpm dev` opens a catalog). `pnpm --dir apps/desktop dev --host 127.0.0.1` runs the Vite-only browser preview at `http://127.0.0.1:1420/` (`?clips=3000` seeds the scale fixture); it uses the CUJ harness as backend and can never validate native dialogs, Tauri-to-Rust calls, the media origin, FFmpeg, persistence, or filesystem permissions. The preview has no media origin, so no clip plays. Never treat a browser-only pass as an end-to-end media test. Builds: `pnpm --dir apps/desktop build`, `pnpm build:cli` (`target/debug/rcdvlog`), `pnpm build` (Tauri bundle), `pnpm build:server`, `pnpm build:web`. Server deployment: `docs/server/deployment.md`; security/pairing: `docs/server/security.md`. Never expose the raw `127.0.0.1:8090` backend or enable Funnel.

Rules that must not be violated:

- **Dev eval bridge** (`RCDVLOG_EVAL_BRIDGE=1 pnpm dev`, `pnpm eval -- '<js>'`) is a remote-code-execution surface. It must remain compiled out under `#[cfg(debug_assertions)]`, disabled unless the exact opt-in is present, bound to loopback only, and closed to every WebSocket handshake carrying an `Origin` header. Never turn it into a release diagnostic, runtime setting, or non-loopback listener. A bridge that cannot bind logs and the app starts anyway.
- **`RCDVLOG_MEDIA_DEBUG=1`** mounts `GET /debug/media` on the media origin (behind its token, `Host` and `Origin` guard); it reports full media paths and must stay compiled out of release. `apps/desktop/src-tauri/tests/release_dev_seams_absent.rs` proves both seams are absent.
- **Fixed ports are contracts; ephemeral ports are implementation.** A discovered listener binds `127.0.0.1:0`, reads the port back, and publishes it through a channel its consumer already reads (media origin, eval bridge). A config-pinned port gets a preflight naming the holder. Test fixtures allocate ports dynamically (`scripts/lib/loopback-port.mjs`). Never add a retry loop or fallback range.
- **Desktop 1420 and web 1421 are contracts**: `devUrl`, the Playwright base URL, and the media origin's allowed origins name 1420, and an origin mismatch fails silently. The `predev` preflight (`scripts/preflight-dev-port.mjs`) names the holder and offers a `kill` line only for a Vite inside this checkout; it never kills anything itself.

Eval-bridge env/address-file and media-debug workflow: [docs/dev-environment.md](docs/dev-environment.md).

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
3. The desktop's persisted `lastCatalogPath`
4. Legacy application-support catalog

Always pass `--catalog` in agent automation, tests, and temporary workflows. A desktop project folder can otherwise be confused with the legacy catalog.

SQLite connections enable foreign keys, WAL journal mode, and a five-second busy timeout. Read operations can continue during an uncommitted writer. Bundled SQLite must stay at 3.51.3 or newer (`rusqlite` 0.39+): 3.51.1 deadlocks when two connections to one WAL database close at the same time, and the catalog opens a connection per operation (guarded by `core/rcdvlog-core/tests/sqlite_concurrent_close.rs`, #258). Project mutations append typed operations with materialized revisions. Adopted projects synchronize bounded operation pages between independent local catalogs; live SQLite/WAL files are never copied or placed on NAS.

Project catalogs use an explicit `schema_migrations` history and are currently at version 66. The server control catalog is version 16; backup format is version 1. Migration definitions have immutable names/source identities and each migration commits atomically. Migration 18 includes a one-time compatibility reconciliation for catalogs created on the import feature branch before its migration IDs were merged with main. When changing the schema:

- Preserve existing catalogs; never solve migration issues by deleting the database.
- Add a forward migration and an upgrade test using an older schema fixture.
- Back up or otherwise provide recovery before any destructive rewrite.
- Keep Rust row order, Serde DTOs, TypeScript mirrors, CLI JSON tests, and queries synchronized.
- Remember that `Catalog::open_existing` validates that `projects` and `assets` exist before opening read/write and migrating.

## 9. Core data and media behavior

Full recipes and measured tables: [docs/core-media-reference.md](docs/core-media-reference.md); read it before changing these areas. Rules not to violate are inline.

### Assets, ingest, chronology

- `Asset` is defined canonically in `core/rcdvlog-core/src/lib.rs` and mirrored in `apps/desktop/src/types.ts`. Availability is computed on read: `originalAvailable` needs a node-local location whose sampled fingerprint was verified; `proxyAvailable` needs the stored proxy path to exist; thumbnails are returned only when the file exists.
- `AssetPatch` is a full annotation replacement (notes, labels, removed, rotation all required). React's `patchFor(asset)` sends the full current state; the CLI fetches the asset, merges optional flags, and sends the full patch. Preserve this on both sides.
- Ingest scans recursively without following symlinks (`mp4 mov m4v mts m2ts avi`, case-insensitive). Identity is a sampled BLAKE3 fingerprint (size plus up to 1 MiB from start, middle, end), unique per `(project_id, fingerprint)`; re-ingest is idempotent. Originals stay at their paths. ExifTool failure degrades to other metadata and must not block ingest.
- Chronology order is `best_time IS NULL, best_time, filename, id`; raw camera timestamp text is preserved. A clock event applies a fixed offset per camera from a lexical filename boundary and never mutates source metadata. The story ribbon spaces clips by ordinal position, not duration (68 px columns, `[`/`]` zoom, `C` centers); do not revert to a fit-all duration plot.

### Proxies and thumbnails

- **Only the original is accepted as proxy input**; an offline original fails explicitly. Candidates are written to unique staging files and validated (H.264, at most 1280 px wide, `yuv420p`, zero display rotation, duration correspondence, AAC audio present, full decode) before atomic commit; a failed transaction never changes `assets.proxy_path`. Proxy version 2 is obsolete.
- `auto` is the safe encoder default and `software` never initializes hardware. macOS tries VideoToolbox (up to four workers) and retries the software recipe on failure; Linux `auto` uses VAAPI only after a bounded synthetic FFmpeg probe proves it, never from device-file presence or an encoder listing alone. The software recipe overrides input rotation to 0, scales to at most 1280 px, `libx264 veryfast` CRF 23, AAC 128 kb/s, `+faststart`.
- FFmpeg progress is ETA display only: only a completed clip updates persisted throughput.
- Proxy and thumbnail writers share cross-process claims in `cache/.representation-locks`; lock files persist and must never be unlinked. A busy CLI writer exits with `representation_busy`; batches count contention as skipped, not failed.
- Only thumbnail quality 1 (JPEG at 20% of duration, at most 480 px, `q1.v2.jpg`) is implemented; qualities 2-10 must keep failing clearly. Input preference is original, then proxy.

### Transcription (local, offline)

- **Audio comes from the proxy, never the original**; a clip with no proxy fails explicitly. Recipe `stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1`, batch key `...-w1` (one worker); a transcript is identified by recipe plus the proxy's sampled fingerprint, so a regenerated proxy makes it stale.
- **Never enable `whisper-rs`'s `metal` feature** (it emits a macOS 15 symbol and will not launch on the documented OS floor); the dependency stays `default-features = false`. CPU-only is deliberate.
- **Probe audio before extracting PCM** with `probe_source_audio_present`; a valid video without audio completes as `no_audio` with zero VAD/ASR work. Probe or extraction failure is `failed` and retryable; corrupt media is never reclassified as silence.
- **The VAD gate is a correctness requirement**: with no detected speech the clip is stored as `no_speech` (reason `no_detected_speech`) and never reaches a model. `no_speech` is a success and must never be conflated with `failed`. Detected speech followed by empty provider output is `no_accepted_speech` (`empty_provider_output`). Quality-gate rejections stay `rejected`. Legacy rows migrate as `legacy_no_speech_unverified` and are review-required.
- Windows are 30 seconds; do not reintroduce 8-25 s chunking. Do not use `no_speech_probability()` as a gate; the local gate is repetition `>= 0.3` or a known token-probability mean below `0.20`. Missing or malformed probability evidence is rejected (`missing_probability_evidence`, `invalid_probability_evidence`), never invented as 0 or 1, and provider confidence or log-probability must not be relabelled as mean token probability.
- **A batch never downloads a model**; it fails naming the file, size, and location. Installation is explicit (global settings or CLI `models install`). Models live in `<data dir>/com.rcd.vlogcutter/models/` (the older per-project `cache/models/` is still read). CLI `models status` and `transcribe batch` semantics: [docs/agent-cli.md](docs/agent-cli.md).

### Clip analysis (opt-in cloud)

- Reads proxies, never originals; sends a deterministic frame pack (recipe `framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1`) plus an accepted transcript when current. Prompt `vlm.observe.v2` with bounded-schema and domain validation. The OpenRouter key comes from `OPENROUTER_API_KEY` or `~/.openrouter.key` and is never stored in a catalog. Qwen is the default with Gemini fallback.
- Output is a suggestion layer: human notes are never touched; label consolidation remaps only labels in the AI ownership table and commits typed operations; title/description use `set_clip_summary`, not `AssetPatch`. Analysis is stale when frame recipe, prompt version, representation, or proxy fingerprint changes.
- Paid analysis is admitted only through a concrete `RunPolicy` ([docs/analysis-preflight.md](docs/analysis-preflight.md)) with a separate authorization record and bounded pilot; frames-only requests never read or send transcripts, project vocabulary needs its own approval, unknown/interrupted usage is fail-closed, and any policy/source/schema/context change invalidates approval. Provider diagnostics persist stable categories only. The legacy runtime ceiling is not cross-process safe; do not claim concurrent safety until runtime wiring calls `analysis::accounting`.
- Cancellation: batch callbacks return `ControlFlow`; `Break` stops new claims without abandoning paid requests, and every stage commits per clip. `moments` and `prepare` CLI adapters take explicit scope, are suggestions requiring review, and never change inclusion, human annotations, or chronology.

### Batch estimation, calibration, publishing, edit planning

- All long batches use `core/batch-estimator` with explicit worker count and versioned task keys; change the key when settings or concurrency change. Failures never contaminate throughput history. Task keys: [docs/batch-estimation.md](docs/batch-estimation.md).
- `rcdvlog calibrate run` is explicit and opt-in; readiness, preflight, import, and analysis never start a benchmark or download a model. One shared local-heavy slot covers proxy/transcode, full-hash, speech, and future local analysis; entrypoints take one advisory same-host filesystem lease and batch internals must not nested-acquire it.
- YouTube: scope is exactly `youtube.upload`; never add read/management scopes; never log tokens, codes, verifiers, or session URLs; never silently recreate an expired session. Callers invoke upload explicitly after a successful render ([docs/youtube-upload.md](docs/youtube-upload.md)).
- Edit-plan revisions are separate from labels and removal; rough-cut inclusion and source ranges live only in the edit-plan surface. Removal controls labeling visibility only.

## 10. Desktop UI conventions

Keyboard-first and dense; domain logic stays out of `App.tsx`. Full API/state, ribbon, CSS, and top-bar-ladder detail: [docs/desktop-ui-conventions.md](docs/desktop-ui-conventions.md).

- `api.ts` selects real Tauri calls when `__TAURI_INTERNALS__` exists, else the harness backend. Optimistic asset updates replace the returned canonical `Asset`; failures roll back and surface an error. Merge completed assets from `proxy-progress`, `thumbnail-progress`, and `analysis-progress` events immediately.
- Review video is **not** served over the Tauri asset protocol (`WKWebView` yields no first frame through a custom scheme). The app binds its own loopback HTTP origin (`media_server.rs`, [docs/tasks/desktop/MEDIA_DELIVERY.md](docs/tasks/desktop/MEDIA_DELIVERY.md), [docs/desktop/HARD_LESSONS.md](docs/desktop/HARD_LESSONS.md)). Its eight security requirements are acceptance criteria with tests; never weaken one or widen the bind: loopback bind, session token, asset ids rather than paths, storage-root re-validation, `Host`/`Origin` checks, `GET`/`HEAD` only, no token or path in logs, stop with the app. `api.mediaUrl` carries the proxy-versus-original choice (`rep=`); on a media `error`, `api.recoverMediaOrigin()` restarts the origin at most once per session.
- Playback controls are custom stage-level UI; do not re-enable native `<video controls>` (they inherit the rotation transform). Playback keeps a bounded three-slot window (previous/current/next); do not widen it without measurement. Autoplay is `never`, `proxyAvailable` (default), or `always`.
- Thumbnails and posters still use the asset protocol (scope: app-data cache, `$HOME/**`, `/Volumes/**`, temp fixtures). If media paths outside it are introduced, update that scope and the loopback origin's storage roots deliberately; never disable security globally.
- Layout: minimum window 980x680, default 1440x900. Keep visible focus, semantic labels, `aria-keyshortcuts`, and the dark editing-bench look. The top bar degrades rather than clips: anything added to it must be bounded in width, and the bar clips across but not down (`overflow-x: clip`).

### Keyboard contract

Preserve and update both visible hints and the `?` keyboard map when shortcuts change.

- `J`/`K` or arrows: next/previous clip. Space: play/pause. `L`: label field (Enter adds). `R`: remove/restore. `N`: focus notes (consume the `n` keydown first). `/`: queue search.
- Notes: Enter saves; Command+Enter newline; Shift+Enter saves and advances; Escape discards the editing session and blurs. After advancing, notes must not remain focused.
- `P` selected proxy; Shift+`P` missing proxies; `T` missing thumbnails; `A` analyze project; `M` add media; `[`/`]` ribbon zoom; `C` center clip; `O` rotate 90 degrees clockwise; Shift+`O` flip 180; `F` Focus; Cmd+`1`/`2`/`3` modes; Shift+`T` Chronology desk.
- Command+`N` new project; Command+`O` open catalog; Command+`,` global settings; `?` keyboard map.

Global shortcuts return early for text inputs, textareas, and selects (except modal Escape). A shortcut that moves focus must call `preventDefault()` first.

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
- Desktop-local error codes are `invalid_arguments`, `invalid_input`, `not_found`, `media_offline`, `media_tool_failed`, `representation_busy`, and `internal_error`; remote server commands preserve the shared server registry.
- `doctor` must not create or mutate the selected catalog.
- `doctor` is observational and does not use the desktop catalog fallback;
  `--check` or repeatable `--require-*` options opt into explicit readiness
  requirements. Workspace refresh/check require an existing explicit catalog
  and a selected absolute executable. They manage only
  `.vlogcutter/cli-binding.json`, never project instruction files.
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

Remote server commands require an explicit `--endpoint`/`RCDVLOG_SERVER_ENDPOINT` and use a paired device token from `RCDVLOG_SERVER_TOKEN`, an owner-only credential file, or interactive Keychain fallback. Never pass a token in argv. Read-only families cover status, projects, library, storage, scans, jobs, sync, representations, credentials, and backups; mutations are deliberately named. `rcdvlog-server admin` owns offline bootstrap, restore, recovery, and redacted diagnostics. See `docs/agent-cli.md` and `docs/server/troubleshooting.md`.

Use `server ... jobs wait --job <id>` for a quiet terminal result instead of
agent-driven polling. `jobs callbacks register/list/deliver` expose durable
completion delivery; the local consumer runs an explicit absolute executable
with literal argv and a JSON stdin envelope. Callback delivery is at least
once: consumers deduplicate durable acceptance by `deliveryId`. Opaque targets,
leases, events and receipts live in the server control catalog; executable paths
and Codex session management remain in the external consumer. See
`docs/server/job-callbacks.md` for timeout, retry and acknowledgement contracts.

## 12. Testing and verification

Run focused local checks for the changed behavior and its dependencies before
PR, then require passing Linux and platform-relevant macOS CI on the exact
candidate head before merge. The conservative skip policy and stable
`ci-required` aggregate are documented in `.github/workflows/README.md`. The
broader automated checks below are supplied by CI; select additional local checks according to the changed boundaries and risk. Manual acceptance
requirements remain explicit.

| Change area | Fast check | Required broader check |
| --- | --- | --- |
| React/TypeScript/CSS | `pnpm --dir apps/desktop check` | `pnpm --dir apps/desktop build` plus interaction/visual check |
| Desktop user journeys | `pnpm test:desktop:cuj` | `pnpm test:desktop` plus interaction/visual check |
| Desktop coverage floor | `pnpm test:desktop:coverage` | thresholds in `apps/desktop/vitest.config.ts`; raise them, never lower them to go green |
| Desktop layout/scroll/focus | `pnpm test:desktop:browser` | headless-Chromium tier; real-engine layout, scrolling, focus, pointer, a11y, and the render-cost budgets in `e2e/scale.spec.ts` — never native evidence |
| Rust core | `cargo nextest run -p rcdvlog-core` | `cargo nextest run --workspace` plus `cargo test --workspace --doc` |
| ETA library | `cargo nextest run -p batch-estimator` | `cargo nextest run --workspace` plus `cargo test --workspace --doc` |
| CLI parsing/JSON | `cargo nextest run -p rcdvlog-cli` | `pnpm test:integration` |
| Tauri commands/settings | `cargo test -p rcdvlog-desktop` | real `pnpm dev` smoke test when native behavior changed |
| Server core/API | `pnpm test:server` | two-node process test plus target-topology check when external behavior changed |
| Chrome web | `pnpm test:web` | `pnpm test:web:e2e` plus real HTTPS visual/interaction check |
| OpenAPI/deployment/docs | `pnpm check:openapi` | `pnpm check:deployment`, `pnpm check:docs`, and Compose render when available |
| Repository scripts in `scripts/` | `pnpm test:scripts` | `pnpm check:docs` |
| Formatting/lints | `cargo fmt --all -- --check` | `cargo clippy --workspace --all-targets -- -D warnings` |
| Full repository | — | `pnpm check` |

CI runs the Rust workspace tests with cargo-nextest (version pinned as `NEXTEST_VERSION` in `.github/workflows/ci.yml`; install the same version locally, e.g. `cargo install --locked cargo-nextest@<version>`), sharded as `linux-rust (tests N/4)` jobs via `--partition count:N/4`, plus `cargo test --workspace --doc` because nextest does not run doctests. `.config/nextest.toml` fails a hung test by name after 5 minutes (`slow-timeout` 60 s period, `terminate-after = 5`). Nothing is skipped or retried.

`pnpm check` runs TypeScript checking followed by all Rust workspace tests. The process-level integration suite creates a real one-second FFmpeg fixture and validates CLI persistence, WAL reads, ingest, metadata, deduplication, labels/notes/removal, clock correction, thumbnails/proxies, and offline-original review. Its media portion skips when FFmpeg or ffprobe is missing.

Gate commands (full usage, JSON focused-check plan format, publication wrapper, and post-publication CI handling: [docs/gate-and-publication.md](docs/gate-and-publication.md)):

- `pnpm gate --scope fast` is iteration-only. `pnpm gate --checks /abs/focused-checks.json` is the default local gate (pinned toolchain, diff checks, Clippy, typecheck, plus the named checks you list); `--scope pre-pr` follows all Linux CI checks and is for specific risk or repeated CI failure.
- `pnpm test:focused <cargo-package> <exact-test-name>` runs one Rust test on the CI pin.
- Publish with `pnpm publish:after-gate push|pr --checks ...` (fast scope is refused).
- Before declaring an issue complete, CI must pass on the exact candidate head; neither an in-progress run nor a green run for an older commit is confirmation. Wait for jobs to finish before fetching failure logs, repair locally, re-gate, and push a new commit.

The CI encoding of this gate table lives in `.github/workflows/ci.yml`: concurrent
Linux Rust lint/tests, frontend and browser jobs, conditional macOS, and the
always-reported `ci-required` aggregate. It runs on every main push and PR, merge
queues, manual dispatch and a weekly unconditional full run. When adding a
new gate command to this table, add a corresponding named step to the workflow. When a gate command is removed or
demoted, remove or adjust its CI step. The table and the workflow must not
drift apart.

The fast desktop CUJ suite (`apps/desktop/src/test/cuj`) boots the real `App`
in jsdom and answers its Tauri calls from a deterministic in-memory backend
installed at `window.__TAURI_INTERNALS__`; `docs/tasks/harness/ARCHITECTURE.md`
is its contract. It runs the real `api.ts`, the real command names and argument
shapes, and the real event subscriptions, so it catches adapter and workflow
regressions cheaply. It runs no browser engine, no Tauri process, no FFmpeg,
and no network, and it is therefore never evidence for any of the boundaries
listed immediately below.

Do not replace these real process/media boundaries with mocks. Conversely, do not pretend headless tests validate:

- DaVinci Resolve import.
- macOS dialogs and filesystem permission prompts.
- Native media playback, including whether an un-played clip paints its first frame.
- Sleep/wake or source-volume disconnect/reconnect.
- Expo-scale NAS performance.
- Ubuntu/Compose restart and real NFS/SMB mount behavior.
- LAN Caddy trust, Tailscale Serve/ACLs, and endpoint failover.
- Target-laptop CPU/RSS/thermal behavior.
- Anything a fast CUJ appears to show about media, chronology, or import:
  that tier fakes the IPC destination, so its media is logical playback
  state, its chronology is a declared outcome, and its import copies
  nothing.

For frontend interaction changes, use the Vite browser preview for focus, keyboard, scrolling, progress, and layout behavior, then use Tauri when the change depends on native APIs. Inspect the actual rendered UI; a passing TypeScript build does not catch focus propagation, overflow, or unreadable density.

## 13. Real-project acceptance fixture

Scale acceptance runs against a large real corpus that is **not** named in this repository; point tooling at it with `RCDVLOG_ACCEPTANCE_MEDIA_ROOT`. Never hard-code its path in Git, an issue, or a commit message (it names somebody's private footage). It is source media, not a disposable test directory: confirm the volume is mounted and permission granted, enumerate read-only first, never write generated files into the source tree, use a catalog under the laptop-local projects root, start with a small subset, and report exact scan/import/failure counts while preserving partial work. It is periodic manual acceptance. Checklist and coverage: [docs/acceptance-fixture.md](docs/acceptance-fixture.md).

## 14. Change recipes

Step lists are in [docs/change-recipes.md](docs/change-recipes.md). In short:

- **New domain operation**: core types and validation with tests, thin Tauri command (registered), `types.ts`/`api.ts` mirror, UI with accessible keyboard/error states, CLI command with stable JSON, process-level integration test, docs.
- **Long-running batch**: versioned task key, explicit worker count, `BatchEstimator` with persisted history, per-item progress and incremental commits, failures excluded from throughput, UI merges each result.
- **Project settings**: Rust `DesktopSettings` with a Serde default, TypeScript mirror and harness fixture, atomic persistence with missing/older/corrupt-file tests, control in global settings.
- **Media generation settings**: treat outputs as a new representation version; change cache and task keys; update docs and tool checks; test real Sony/DJI/iPhone/Insta360 H.264/H.265 media.

## 15. Safety and working-tree discipline

- Inspect `git status` before editing. The repository may contain uncommitted or untracked user work; preserve it.
- After completing and verifying a meaningful milestone, commit the full scoped change set unless the user explicitly asks not to. Leave the repository clean so milestones remain easy to inspect or revert.
- Keep edits scoped. Do not reformat large unrelated files, especially the current large single-file Rust core and React app.
- Never delete catalogs, source media, project folders, proxies, thumbnails, or exports unless the user explicitly requests the exact destructive action.
- Tests must use `tempfile`/temporary directories. Never point destructive test setup at `~/Movies/VideoProjects`, `/Volumes`, or the repository root.
- Do not edit SQLite by hand to achieve product behavior. Add a domain operation and migration.
- Do not use the browser preview or the CUJ harness as evidence that persistence or media processing works.
- Do not use local Playwright as evidence that Caddy, Tailscale, Ubuntu, native Tauri, or NAS checks passed.
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
- Server changes preserve loopback-only networking, authenticated APIs, read-only original binds, path containment, local-SSD SQLite, and explicit new-root restore.
- OpenAPI drift, deployment invariants, documentation links, and the applicable external acceptance boundary are checked and reported.
