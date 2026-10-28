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

Implemented now:

- One portable SQLite catalog per newly created desktop project.
- Configurable projects root, defaulting to `~/Movies/VideoProjects`.
- Project library discovery and loading an existing catalog.
- Reference-in-place recursive ingest of supported media extensions.
- A desktop smart-ingest dialog that classifies a source before mutation,
  states the project-scoped delta and resolved destination, checks copy size
  against free space, creates/rebinds named archive roots, copies removable
  cards atomically with optional full-file verification, remembers the proxy
  preference, supports cancellation/resume, and keeps a file ledger available
  after the dialog closes.
- Smart ingest shows the resolved camera before import, lets the user choose an
  existing or new project camera, uses that choice for both asset ingest and
  sequence preview, and remembers it only when card layout, filename prefix,
  and representative EXIF make/model form an unambiguous project-local key.
- Low-frequency macOS removable-card detection with a review/dismiss banner;
  it lists and counts only until the user asks to review the import.
- Sampled fingerprint deduplication within a project, with rediscovered content
  repairing an asset whose original went offline rather than being discarded.
- Verified camera `.LRF` sidecars can resolve through recorded same-asset
  alternate originals, with persisted CFR integer-reduction time mappings and
  bounded content checks. `camera-proxy frame-pack` samples that explicit camera
  representation offline; generated-proxy readiness remains separate. See
  `docs/camera-proxy-sidecars.md`.
- Verified node-local media locations: availability requires that the bytes were
  checked, so a reformatted card reusing a pathname cannot bind an old asset to
  new footage.
- ffprobe technical/timestamp metadata and ExifTool camera make/model lookup.
- Label queue, playback, free-form labels, notes, reversible removal, and keyboard shortcuts.
- Fixed camera offset events from a filename boundary onward.
- Operation-backed capture sequences, clock epochs, clip-time exceptions,
  clip-anchored trip phases, event-order constraints, a deterministic
  chronology solver, and durable diagnostics. A ribbon phase boundary follows
  its anchor clip when later camera-clock corrections change solved order.
- Read-only chronology/diagnostics CLI commands and basic desktop chronology
  and edit-plan drawers.
- Versioned edit-plan persistence and validation, explicit source-audio ranges,
  J/L placement and short audio fades in local previews (legacy omitted audio
  remains linked), identity-based audio diffs, and an explicit OTIO rejection
  until finishing can preserve these edits.
- Low-resolution preview
  generation, and deterministic pure-Rust OTIO writing.
- Local preview renders have schema-54 node-local attempt and verified encoded
  segment receipts, bounded progress/status reads, explicit cancellation, and
  opt-in byte/input verification. They are derived evidence, not synchronized
  decisions; see `docs/edit-plan-preview-artifacts.md`.
- Schema 57 adds separately labeled published token estimates to shared analysis
  accounting; estimates consume the same budget without becoming provider-reported
  charges. Existing transports retain their admission rules. See `docs/analysis-accounting.md`.
- Schema 58 adds node-local native-video carriers, request evidence and run/window
  details. The explicit direct Gemini route shares canonical policy approval,
  bounded capability pilots, reservation/dispatch/receipt accounting and the
  existing moment artifact owner. Source scopes and video/audio/transcript
  payloads are explicit; no provider/model/tier fallback or automatic resend is
  permitted. Local media preparation is not an upload permit. See
  `docs/native-video-review.md`; fake protocol tests do not establish live
  provider capability or human listening/pacing acceptance.
- Schema 56 adds optional immutable chapter selection intent, pacing profiles,
  evidence identities and reports. Selection reuses local moments, preserves human
  constraints and creates inactive normal plans; see `docs/edit-plan-selection.md`.
- Schema 55 adds portable revision-bound edit-review notes, proposals, human
  decisions and target mappings. Only an explicit accepted proposal can be
  applied, producing a normal inactive edit-plan draft; feedback does not
  activate plans or change chronology. See `docs/edit-plan-feedback.md`.
- Quality-1 thumbnails and 720p H.264/AAC proxies with four concurrent
  VideoToolbox workers on macOS and a one-worker software fallback elsewhere.
- Proxy-only cloud clip analysis with deterministic frame packs, transcript
  fusion, bounded observations, provider fallback, operation-backed AI labels,
  quality-10 representative thumbnails, and explicit label consolidation.
- A desktop analysis surface: one cancellable proxies/transcription/analysis
  chain with a pre-run cost projection and per-run budget ceiling, per-run spend
  accounting, generated titles as the clip's display name, transcript and
  observation review panels, transcript-aware queue search, unusable-clip
  triage, and Keychain-stored OpenRouter credentials.
- Weighted batch progress and ETA with project-persisted rate history.
- Review video served from a token-guarded loopback HTTP origin the desktop app
  binds at startup, replacing the Tauri asset protocol for video so an un-played
  clip has a frame to paint. Range-streamed from disk, resolved by asset id
  through the open catalog, and stopped with the app.
- A main view laid out for a 13" MacBook Air (1440x900). The shell is exactly
  three rows — topbar, workbench, full-width story ribbon — and two invariants
  are enforced by tests rather than by review: the right rail holds only
  human-authored content (notes, labels, title, removal, plus the AI
  suggestion strip and the unusable chip), and the notes field has fixed
  geometry that nothing else may move. Machine output lives in a tabbed
  evidence shelf beneath the player, collapsed by default. Chronology and edit
  planning are full-window modes on Cmd+1/2/3. Focus is the only fullscreen
  concept — the transport's ⤢ enters it with the overlays down and leaves it
  again, and native/DOM fullscreen is gone. A Focus presentation (F) fills
  the window with the frame and floats the notes card, preserving the caret and
  the playback position because it restyles the mounted tree rather than
  replacing it. See `docs/superpowers/specs/2026-08-04-desktop-review-layout-design.md`.
- One owner for the open project's reads (`apps/desktop/src/session/projectSession.ts`):
  the queue, chronology overview, analysis index, readiness, and the selected
  clip's evidence, with explicit `reload`/`reloadAssets`/`reloadChronology`/
  `reloadReadiness`/`reloadEvidence` invalidation. Every read carries the
  session that started it, so a response for a project the user has left cannot
  write state — including its error and loading state. A failed queue read is
  retryable in the interface (queue heading reload control, and *Try again* in
  the empty queue) rather than only by relaunching.
- One owner for the asset write path
  (`apps/desktop/src/session/assetMutations.ts`): optimistic apply, a stated
  failure policy per mutation — `rollback` for labels, removal, and rotation;
  `retain-draft` for human-authored notes — save state, a double-submit guard
  that refuses an identical in-flight write, and error surfacing. A note whose
  save was refused stays on screen marked *Unsaved* with a *Retry* that sends
  the same draft, and survives reloads because retained drafts are written back
  over what a read returns. Judgments are still one whole-`AssetPatch` write
  through `update_asset`; the narrow single-field adapters were removed and the
  decision is recorded in `apps/desktop/src/test/harness/contract.ts`.
- Relative-order “story ribbon” with camera lanes sized from the project's
  actual camera count, horizontal scrolling, zoom, and a merged single-lane
  variant for Focus. The expanded Chronology desk uses the same camera-first
  hierarchy, with selectable capture-sequence/card runs inside each lane.
- An explicit camera-lane identity merge previews affected clips and card runs,
  then moves the source identity into an existing lane without combining its
  capture sequences or changing clock-epoch IDs. The typed operation is
  replay/baseline safe, updates Smart ingest mappings, and derives flattened
  aliases deterministically from all merge facts so later operations carrying
  an old identity and concurrent chained merges converge.
- A non-interactive JSON CLI for catalog, clip annotation, chronology, and media operations.
- Immutable typed project operations for asset membership, notes, labels, removal, rotation, and fixed clock events, with deterministic materialized views and conflict diagnostics.
- Read-only catalog/project/clip/proxy diagnostics, including the desktop's persisted last-selected catalog, project, and asset breadcrumb.
- Optional Ubuntu companion runtime with a local-SSD control catalog (schema 11), local project replicas, logical storage roots/node bindings, a global verified-content library, and scoped availability observations.
- Durable leased jobs, cancellation/retry/restart recovery, scans/schedules, full hashing, shared proxy/thumbnail generation, node-scoped ETA history, and authenticated byte-range streaming.
- Baseline format 1 and sync protocol 1.0 with bounded operation pages, per-origin cursors/acknowledgements, deterministic conflict handling, WebSocket hints, and polling recovery.
- Browser bootstrap/session auth, revocable desktop credentials, Keychain storage, CSRF/Host/Origin/proxy validation, LAN Caddy and Tailscale Serve deployment assets.
- Chrome library/project browse, add-to-project, proxy review, annotations, storage/jobs/sync/credentials administration, and stable polling fallback.
- Authenticated server CLI families plus local-only bootstrap, restore, sequence-recovery, and redacted diagnostic commands.
- Verified scheduled/manual backups of server databases with manifest-last publication, integrity/checksum validation, retention, and new-root-only restore.

Important gaps; do not accidentally describe these as complete:

- Import preflight includes a read-only, per-run capture-sequence decision with
  volume, folder, filename-prefix, counter-reset, and time-discontinuity
  evidence. The Chronology desk supports explicit split/merge and single-clip
  or clip-and-following reassignment; preflight never applies those proposals.
  A counter restart is presented on the desk as a question with two answers —
  split into a new card, or record "same card" durably — rather than as a
  problem, because swapping cards through one camera is ordinary practice. The
  "same card" answer is an operation (`captureSequenceSplitDeclined`) and is
  reversible from the desk's Card tab.
- Project deletion and cache cleanup now ship (#16). Deletion runs from the
  opening project library behind a removability audit — refusing a folder that
  holds an archive root, a referenced original, another project's rows, or any
  file the app did not create — and a typed project-name confirmation;
  `delete_project` removes trip phases before assets because their clip anchors
  are `ON DELETE RESTRICT`. Cache cleanup is a Settings storage panel that lists
  the seven cache categories with their weight, replacement cost, and what stops
  working until rebuilt; there is no single clear-cache button, and cleanup
  reads nothing about the media so it works with every drive unplugged. There is
  no general mounted-volume inventory UI. Relink now
  exists: media locations carry a verification state, ingest repairs an asset
  whose original it rediscovers, and an offline-volume diagnostic offers
  `Locate…` (CLI: `relink`). Queue rows carry a compact offline mark that
  separates *original offline, reviewable from its proxy* from *nothing to
  play*, and the selected clip's File shelf reaches the same project-wide
  relink, so repair no longer requires knowing the chronology desk's
  diagnostics exist. Removable-card detection and archive-root rebinding
  are implemented; see `docs/tasks/import/EPIC.md` for the remaining physical
  card/volume acceptance gate.
- Full-patch writes carry an optimistic `expectedRevision`. `Asset.revision` is
  the materialized operation revision; `update_asset` refuses a patch built
  from a stale read, and the desktop recovers by re-reading and re-applying the
  user's own change on top of what the catalog now holds. The CLI takes
  `--expected-revision` and reports `stale_revision`. What this does *not* do:
  tell whose value won when both writers edited the same field — the client
  cannot distinguish its own optimistic value from the other writer's, so an
  explicit edit wins that field and the other writer's other fields survive.
  Cross-node operation sync is unchanged.
- Search is currently simple filename/camera/label/note/transcript filtering,
  not SQLite FTS. Result rows say *why* each clip matched — the field and a
  marked snippet — and Enter in the search box cycles the selected clip's
  playhead through the places the phrase was spoken, marking them in the
  transcript. Only the flattened transcript text is indexed per clip, so the
  per-row snippet has no timestamp; the timings come from the open clip's own
  transcript.
- The label field offers the project's existing vocabulary as a keyboard-first
  typeahead, and a commit that differs from an existing label only by case
  joins that label rather than opening a second one. Nothing is renamed,
  merged, or deleted: `apps/desktop/src/review/labelVocabulary.ts` is a
  read-only view over the labels already written.
- Sparse suggested key moments and a bounded preparation evidence CLI now exist;
  they do not certify arbitrary cut ranges or replace human review. Separate chapter/event/script entities remain unimplemented; optional sequence
  intent is stored with immutable edit-plan revisions. Edit-plan revisions, validation,
  preview rendering, and OTIO writing exist, but the desktop edit-plan workflow
  still lacks reordering, trimming, removal, preview playback, export history,
  and overwrite confirmation. The supported `edit-plan` CLI exposes
  list/get/create/diff/validate/activate/preview/inspect. Bounded source inspection
  stores actual decoded PTS, source/excerpt mapping, oriented frames and optional
  playable sound/waveform as derived SQLite receipts. It never accepts a range
  or changes human judgments; see `docs/edit-plan-inspection.md`. Explicit human
  picture/audio allow/exclude constraints are operation-backed, revision-guarded
  and honored by validation, preview publication and advisory utterance trims;
  see `docs/edit-plan-source-ranges.md`.
- No materialized project-local agent workspace. The CLI can report its build
  and readiness, and can explicitly bind/check the selected executable and
  catalog in `<project-root>/.vlogcutter/cli-binding.json`. This binding never
  generates or edits project instructions. Materializing `AGENTS.md`,
  `PROJECT.md`, `script.md`, `project.json`, and `edit-plan.json` into the
  video project root remains planned under issue #48 and PRD Milestone 5.
  Script revisions do not exist either.
- Trip phases and timezone display are persisted and solved, and the ribbon can
  add a location/transit phase forward or backward from a hovered clip, edit an
  existing phase, and atomically cut a new phase from either side of a filled
  segment. The Chronology desk adds identity-snapped pointer and keyboard
  boundary adjustment for both phase kinds, plus a full camera-lane/card-run
  and trip-band view. Story beats remain display-only browser-mock fields and are not
  persisted.
- The Chronology desk surfaces conflicts, explanations, diagnostics, clock
  epochs, phase boundaries, split/merge, single/following reassignment, and
  bounded solver-owned before/after impact summaries. The compact Review ribbon
  deep-links into it and Shift+T opens it with the Review zoom and viewport.
  Dedicated clip-exception and event-order-constraint controls remain domain/API
  capabilities rather than first-class desk tools.
- The desk shows the footage. A compare deck plays the focused clip with the
  clips the solver placed either side of it — either side in *solved* order,
  across cards — as stills that promote to focus on click or with the arrow
  keys. Canvas tiles are thumbnails carrying an ordinal, with the time revealed
  on hover and focus and the full value in `title` and the accessible name; a
  clip with no thumbnail keeps a text tile. The ordinal strip above the lanes is
  a time ruler, marking day changes and timezone changes, the latter because a
  trip crossing zones makes the displayed wall clock run backwards.
- Web has automated Vitest and Chrome Playwright coverage. Desktop React now has Vitest + jsdom + Testing Library component tests under `apps/desktop/src/components/__tests__/`, but there is still no automated native Tauri UI suite.
- No macOS signing/notarization or clean-machine packaging acceptance.
- The transcript shelf follows playback: the segment under the playhead is
  marked, the list scrolls only when that segment has left the viewport, and a
  manual scroll suspends following for five seconds or until *Follow playback*.
  Real scroll geometry is covered by `review/transcriptFollow.ts`'s unit tests
  rather than by a browser tier — the Vite preview has no media origin, so no
  playhead there can actually move.
- Analysis suggestions are shown and editable in the desktop, but there is no
  per-suggestion accept/reject workflow: an AI title or description is simply an
  editable field, and a forced re-run overwrites it.
- Native media playback, filesystem permission prompts, sleep/wake, and NAS-scale behavior remain manual acceptance checks. The loopback media origin's three webview-observable acceptance criteria — an un-played clip's first frame, non-zero `drawImage` alpha before `play()`, and seeking to an unbuffered region — have not been run. Resolve import returns as a later edit-planning acceptance check.
- Ubuntu/Compose, physical NFS/SMB outage behavior, Caddy/Tailscale HTTPS, native pairing/playback, and target resource/thermal acceptance remain unrun; see `docs/tasks/server/ACCEPTANCE_TASK_19.md`.

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
│   │   ├── src/preparation_commands.rs # Thin bounded preparation adapter
│   │   ├── tests/cli_workflow.rs     # Process-level CLI/media integration tests
│   │   └── tests/preparation_workflow.rs # Preparation CLI contracts
│   ├── desktop/
│   │   ├── src/                      # React UI, API boundary, types, styles
│   │   └── src-tauri/                # Tauri commands, settings, native entry point
│   ├── server/                       # Axum runtime, admin CLI, HTTP/process tests
│   └── web/                          # Authenticated Chrome review/admin app
├── core/
│   ├── rcdvlog-core/src/lib.rs       # Canonical catalog, media, annotation, chronology logic
│   ├── rcdvlog-core/src/operations.rs # Typed operations, HLC, materialization, conflicts
│   ├── rcdvlog-server/               # Server catalog, storage, jobs, auth, media, backup
│   ├── rcdvlog-sync/                 # Baseline/reconciliation DTOs and apply logic
│   └── batch-estimator/src/lib.rs    # Domain-independent weighted ETA library
├── packages/
│   ├── project-client/               # OpenAPI snapshot and typed browser client
│   └── review-ui/                    # Shared review components/types
├── deploy/server/                    # Dockerfile, Compose, Caddy, Tailscale, example env
└── docs/
    ├── agent-cli.md                  # Full CLI and JSON contract
    ├── batch-estimation.md           # Estimator semantics and persistence
    ├── integration-testing.md        # Automated versus manual acceptance boundaries
    └── server/                       # Current server operator/developer contracts
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

Minimum documented environment:

- Apple Silicon Mac, macOS 13+
- Xcode command-line tools
- Rust 1.85+ and Cargo.
- Node.js 22+ and pnpm 10.
- FFmpeg 7+ and ffprobe on `PATH`
- ExifTool on `PATH`

The full local Task 19 gate was verified on 2026-07-28 with Rust/Cargo 1.97.1, Node 24.14, FFmpeg/ffprobe 8.1.2, ExifTool 13.36, and Chrome 150. Docker was unavailable, so the target Ubuntu/Compose topology is not accepted.

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

The full pre-PR gate uses the CI-pinned toolchain: Rust 1.97.1,
Node.js 24.11.0, and pnpm 10.17.0. When running that gate, install its Rust
components and package manager with:

```bash
rustup toolchain install 1.97.1 --component rustfmt --component clippy
corepack prepare pnpm@10.17.0 --activate
pnpm install --frozen-lockfile
```

Use a Node.js version manager or the official Node installer to select
24.11.0. Do not let a missing pinned tool silently fall back to another Rust
version; `pnpm gate` verifies and uses the Rust pin before running Rust checks.

Find Cargo with `command -v cargo` and `command -v rustup`; for a standard
installation, load `~/.cargo/env` or add `~/.cargo/bin` to `PATH`. If a host
uses a project-local installation, explicitly set `CARGO_HOME` to its Cargo
directory, `RUSTUP_HOME` to its Rustup directory, and prepend
`$CARGO_HOME/bin` to `PATH` before installing or invoking the pinned toolchain.
These directories are host configuration, not paths to assume in a checkout.
For example, an observed video workspace used `data/tools/cargo` and
`data/tools/rustup`; other workspaces need not contain either directory.
Never substitute the installed stable formatter when the CI pin is absent.

Rust dependencies are resolved by Cargo from the checked-in `Cargo.lock`.

For an agent development binding, build the CLI from the intended checkout
with `rustup toolchain install 1.97.1 --component rustfmt --component clippy`
and `cargo +1.97.1 build --locked -p rcdvlog-cli`. Respect the checkout's
`CARGO_TARGET_DIR` and pass the resulting binary's absolute path to
`workspace refresh`; `workspace check` verifies that exact path, build
metadata, and digest and never resolves a replacement executable.

## 7. Running the app

Run the real Tauri desktop application:

```bash
pnpm dev
```

Open a specific existing catalog at startup:

```bash
RCDVLOG_CATALOG=/absolute/path/to/catalog.sqlite3 pnpm dev
```

For defects that only reproduce inside the live native webview, the desktop has
a dev-only eval bridge. It is a remote-code-execution surface and must remain
compiled out under `#[cfg(debug_assertions)]`, disabled unless the exact opt-in
is present, bound to loopback only, and closed to every WebSocket handshake
carrying an `Origin` header. Never turn it into a release diagnostic, runtime
setting, or non-loopback listener.

```bash
RCDVLOG_EVAL_BRIDGE=1 pnpm dev
pnpm eval -- 'document.readyState'
pnpm eval -- --file /absolute/path/to/probe.js
```

The bridge binds an ephemeral port and publishes it two ways: the marker line
`[RCDVLOG_EVAL_BRIDGE_COMPILED] listening on ws://127.0.0.1:<port>` on the app's
stdout, for a harness that spawned the app, and a JSON address file for a client
that did not. `pnpm eval` finds the file on its own; `--port` and `--wait` are
there for tooling. Set `RCDVLOG_EVAL_BRIDGE_ADDRESS_FILE` to give a concurrent
run its own address file, and `RCDVLOG_EVAL_BRIDGE_PORT` to pin the listener for
a tool that can read neither. A bridge that cannot bind logs and the app starts
anyway: no dev convenience may keep the app from launching.

**Fixed ports are contracts; ephemeral ports are implementation.** A listener
whose address is discovered at runtime binds `127.0.0.1:0`, reads the assigned
port back, and publishes it through a channel its consumer already reads — the
media origin (`media_server.rs`) and the eval bridge both do exactly this. A
config-pinned port gets a preflight that names the process holding it. Test
fixtures always allocate dynamically (`scripts/lib/loopback-port.mjs`). Never
add a retry loop or a fallback range: a listener that comes up somewhere its
consumers are not looking is a silent failure, which is worse than a named bind
error.

The desktop's 1420 and the web app's 1421 are contracts — `devUrl`, the
Playwright base URL, and the media origin's allowed origins all name 1420, and
an origin mismatch fails *silently*. Their `predev` preflight
(`scripts/preflight-dev-port.mjs`) turns a raw `EADDRINUSE` into the holder's
command and pid, and offers a `kill` line only for a Vite whose working
directory is inside this checkout. It never kills anything itself, and it never
claims a process it cannot positively identify.

The loopback media origin has a second, narrower dev-only seam under the same
rules, for the case where a clip will not play and the app is the only thing
that knows why. `RCDVLOG_MEDIA_DEBUG=1` mounts `GET /debug/media` on the origin
— behind its existing token, `Host` and `Origin` guard — and publishes the
origin on `window` so the eval bridge can find its ephemeral port and token.

```bash
RCDVLOG_EVAL_BRIDGE=1 RCDVLOG_MEDIA_DEBUG=1 pnpm dev
pnpm eval -- 'window.__RCDVLOG_MEDIA_ORIGIN__.debugUrl'
curl -s '<that url>&asset=<asset id>' | jq .probe
```

It reports the last 50 requests and, for a named asset, exactly why resolution
would fail — `unknown_asset`, `no_representation`, `missing`, `not_a_file` or
`outside_roots` — which the media route flattens into a bare 404. It reports
full media paths, so it must stay compiled out of release like the eval bridge;
`apps/desktop/src-tauri/tests/release_dev_seams_absent.rs` proves both are.

The client prints JSON values and structured JavaScript errors. Every probe is
logged by the running app. See `docs/tasks/desktop/DEV_EVAL_BRIDGE.md` for the
contract, tests, and release-binary absence check.

The Vite-only browser preview is useful for fast layout and interaction work:

```bash
pnpm --dir apps/desktop dev --host 127.0.0.1
```

It runs at `http://127.0.0.1:1420/`. Append `?clips=3000` to seed a project at
the scale section 1 sets instead of the fourteen-clip demo; that is what
`e2e/scale.spec.ts` boots to measure what the queue and the ribbon cost to
mount. Outside Tauri, `main.tsx` installs the CUJ harness (`apps/desktop/src/preview/`) as the backend, so the preview exercises the real adapters against the same fake backend the CUJ suite drives. `api.ts` and `chronologyApi.ts` have no environment branches left, and a behaviour change in `apps/desktop/src/test/harness/commands/` shows up in both the preview and the CUJ suite. Progress and cancellation are driven on real timers by `src/preview/liveJobs.ts`, which only the preview loads.

It still cannot validate native dialogs, Tauri commands reaching Rust, the loopback media origin, FFmpeg child processes, persistence, or filesystem permissions. The preview has no media origin, so no clip plays. Never treat a browser-only pass as an end-to-end media test.

Build outputs:

```bash
pnpm --dir apps/desktop build       # TypeScript + Vite frontend
pnpm build:cli                      # target/debug/rcdvlog
pnpm build                          # Tauri application bundle
```

`pnpm build` may require the native macOS/Tauri packaging environment and takes longer than the frontend build.

Build the companion server and web application with `pnpm build:server` and `pnpm build:web`. Production deployment is documented in `docs/server/deployment.md`; security/bootstrap/pairing is in `docs/server/security.md`. Never expose the raw `127.0.0.1:8090` backend, enable Funnel, or treat the desktop/browser mocks as server acceptance.

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

SQLite connections enable foreign keys, WAL journal mode, and a five-second busy timeout. Read operations can continue during an uncommitted writer. Project mutations append typed operations with materialized revisions. Adopted projects synchronize bounded operation pages between independent local catalogs; live SQLite/WAL files are never copied or placed on NAS.

Project catalogs use an explicit `schema_migrations` history and are currently at version 57. The server control catalog is version 11; backup format is version 1. Migration definitions have immutable names/source identities and each migration commits atomically. Migration 18 includes a one-time compatibility reconciliation for catalogs created on the import feature branch before its migration IDs were merged with main. When changing the schema:

- Preserve existing catalogs; never solve migration issues by deleting the database.
- Add a forward migration and an upgrade test using an older schema fixture.
- Back up or otherwise provide recovery before any destructive rewrite.
- Keep Rust row order, Serde DTOs, TypeScript mirrors, CLI JSON tests, and queries synchronized.
- Remember that `Catalog::open_existing` validates that `projects` and `assets` exist before opening read/write and migrating.

## 9. Core data and media behavior

### Assets and patches

`Asset` is defined canonically in `core/rcdvlog-core/src/lib.rs` and mirrored in `apps/desktop/src/types.ts`. Availability is computed on read:

- `originalAvailable`: at least one node-local location exists and its sampled
  fingerprint has been verified against the asset identity.
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

Proxy generation uses one shared explicit encoder policy. `auto` is the safe
default; `software` never initializes hardware. Linux `auto` first runs a bounded
synthetic FFmpeg probe and only then may use the VAAPI full-clip
decode/scale/H.264-encode path. Device-file presence or an encoder listing alone
never marks hardware usable. Capability JSON distinguishes `device_unavailable`,
`device_permission_denied`, `ffmpeg_encoder_unavailable`,
`initialization_failed`, and `usable`; `proxy probe --policy auto|software`
exposes it. `proxy provenance --asset <id>` exposes durable attempt/fallback
evidence, and `proxy history` exposes encoder/worker-scoped rates.

On macOS up to four local workers run concurrently. Each first tries
VideoToolbox hardware decode and H.264 encode with the CPU scaler retained for
materially better downscale quality. The hardware settings are equivalent to:

- `-hwaccel videotoolbox` with H.264 `h264_videotoolbox` High profile.
- VideoToolbox constant quality 36 and `power_efficient=1`.
- Scale down to at most 1280 px wide while preserving aspect ratio.
- AAC 128 kb/s and `+faststart`.

If VideoToolbox is unavailable, busy, or rejects an input, the Mac retries the
existing software recipe automatically. On Linux the VAAPI recipe initializes
one render node for hard decode, `-filter_hw_device`, `scale_vaapi`, and
`h264_vaapi` at QP 23. A hardware process or validation failure removes only the
unique staging output and retries software. The Ubuntu companion server reads
`RCDVLOG_PROXY_ENCODER_POLICY` (default `auto`) and persists actual encoder,
attempt, fallback, and validation evidence in server job details. The standard
software recipe remains:

- Override the input display rotation to 0 and disable autorotation, preserving raw encoded orientation without copying the camera display matrix.
- Scale down to at most 1280 px wide while preserving aspect ratio.
- H.264 `libx264`, `veryfast`, CRF 23, `yuv420p`.
- AAC 128 kb/s and `+faststart`.

FFmpeg proxy processes emit output-timestamp progress once per second. Convert
that duration fraction into partial source-byte work so active clips reduce the
ETA before they finish. Partial work is display/ETA state only and must not
update persisted throughput; only a successful completed clip contributes its
full bytes and worker duration. Reset partial work if VideoToolbox fails before
starting the software fallback, and retain completion-only behavior when FFmpeg
does not provide a usable timestamp.

Only the original is accepted as proxy input. If the original is offline, proxy generation fails explicitly. Generated candidates are written to unique staging files and validated for H.264, at most 1280 px wide, `yuv420p`, zero display rotation, source-time duration correspondence, source-audio presence as AAC, and successful video+audio decode before atomic commit. Each transaction writes `proxy_generation_provenance` (recipe, actual encoder, device, policy, timing, ordered failures, fallback flag, validation, timestamp) in canonical SQLite; a failed transaction never changes the registered `assets.proxy_path`. Version-2 proxies are obsolete because they may retain the source display matrix; a later “missing” run regenerates them and skips only valid current-version paths that still exist.

Proxy and thumbnail writers share cross-process claims in the managed `cache/.representation-locks` directory. Claims use the canonical cache root, asset ID, and representation kind; lock files persist after release and must never be unlinked. SQLite remains canonical. A busy single-item CLI writer exits with JSON code `representation_busy`; batch writers count contention as skipped work without recording a failure or throughput sample. Proxy downloads claim before requesting content and stream into unique staging paths, so a stale writer cannot overwrite a later transaction.

### Thumbnails

Only quality 1 is implemented. It is a JPEG at 20% of clip duration, at most 480 px wide, generated to a unique staging file and atomically renamed to `q1.v2.jpg`. Input preference is original, then available proxy. Qualities 2–10 are reserved and must continue to fail clearly until intentionally implemented.

Q1 thumbnails and analysis-selected Q10 representative thumbnails use the same cross-process thumbnail claim as desktop and sync work. See the proxy section above for busy-result and persistent-lockfile behavior; keep the existing output names and recipes unchanged.

### Transcription

Local speech-to-text ships. It runs entirely offline with no API key and never
touches the network. Design and measurements:
[spec](docs/superpowers/specs/2026-08-03-stt-vlm-clip-analysis-design.md),
[round-1 spike](docs/spikes/2026-08-03-stt-analysis-spikes.md).

- **Audio comes from the proxy, never the original.** The original may be
  offline on the NAS, and transcription must work whenever proxies exist. A clip
  with no proxy fails explicitly rather than falling back.
- **Recipe key**: `stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1`. A
  transcript is identified by that key plus the sampled BLAKE3 fingerprint of the
  proxy it came from, so regenerating a proxy makes its transcript stale.
- **Batch key**: `stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1-w1`,
  one worker. Peak RSS is 1.15–1.38 GB and concurrent transcription alongside the
  four-worker VideoToolbox proxy pipeline has never been measured.
- **CPU-only, deliberately.** Do not enable `whisper-rs`'s `metal` feature: ggml
  gates `MTLResidencySetDescriptor` on the SDK rather than the deployment target,
  so a Metal build emits a macOS 15 symbol and will not launch on the documented
  OS floor. The dependency is declared `default-features = false`.
- **Probe audio before extracting PCM.** Transcription reads the proxy and
  calls the reviewed `probe_source_audio_present` FFprobe helper first. A valid
  video with no audio stream completes as `no_audio` with zero VAD/ASR/model
  work and is reused like any other terminal transcript. A probe or extraction
  failure stays `failed` and retryable; corrupt media is never reclassified as
  silence.
- **The VAD gate is a correctness requirement, not an optimization.** Given
  silence, whisper invents fluent speech at high confidence. A clip with no
  detected speech is stored as `no_speech` with reason `no_detected_speech` and
  is never handed to any model. `no_speech` is a success and must never be
  conflated with `failed`, or every silent clip is retried forever. Detected
  speech followed by empty or unusable provider output is
  `no_accepted_speech` with reason `empty_provider_output`, not silence.
  Quality-gate rejections remain `rejected`; exact proxy fingerprints,
  detected-speech duration, decode-window count, raw provider segment count,
  mean token probability and repetition ratio stay attached to the outcome.
  Token probability is optional and stored as SQL NULL when a candidate has no
  comparable measurement; missing evidence is rejected explicitly rather than
  invented as confidence 0 or 1. Provider confidence and average log
  probability are different native metrics and must not be relabelled as a
  mean token probability.
- **Legacy compatibility is explicit.** Rows written before audio probing and
  provider-output distinctions existed keep status `no_speech` but migrate with
  reason `legacy_no_speech_unverified`. Preparation marks those rows
  review-required because they cannot prove verified silence. Transcript
  coverage reports `no_audio` and `no_accepted_speech` separately.
- **Windows are 30 seconds.** whisper bills a full window per decode regardless
  of content, so speech regions are packed into as few windows as possible. Do
  not reintroduce the PRD's 8–25 second chunking: measured, it ran 30% slower on
  39% less audio.
- **Do not use `no_speech_probability()`** as a quality gate. Measured median
  0.000 for good and bad transcripts alike. The local gate that works is
  repetition at `>= 0.3` or a known token-probability mean below `0.20`.
  Non-empty candidate output with any missing probability evidence is rejected
  as `missing_probability_evidence`; malformed provided values are rejected as
  `invalid_probability_evidence`. Unknown evidence must not dilute a known bad
  mean into a pass.
- **A batch never downloads a model.** It fails with an actionable error naming
  the file, its size, and where it belongs. Installation is a separate, explicit
  user action: global settings and CLI `models install` share
  `install_speech_models_with_progress`. Preserve this split; a 574 MB
  transfer must never begin as a side effect of pressing Analyze.
- Models live in a shared application-support directory
  (`<data dir>/com.rcd.vlogcutter/models/`) so one copy serves every project. The
  older per-project `<project folder>/cache/models/` location is still read when
  it holds the model, so existing installs are not re-downloaded.
- CLI `models status` reads identity, integrity, readiness and managed location
  without catalog writes, downloads or engine initialization. CLI `transcribe
  batch` validates an exact project-owned selection, reuses current non-failed
  transcripts, and opens one engine for pending clips. Progress includes per-asset
  outcomes and a one-worker estimate; cancellation preserves completed work and
  failures remain retryable. Existing single-clip and whole-project commands stay
  compatible. See `docs/agent-cli.md`.

**Measured end to end**, release build, M3 MacBook Air (fanless), one worker,
273-clip Osaka/Expo project, 129.1 minutes of footage:

| Measure | Value |
| --- | --- |
| Wall time | **31.5 min** |
| Clips with speech | 131 |
| Clips gated as `no_speech` | 135 (49%) |
| Rejected by the hallucination gate | 7 (5 repetition, 2 low token probability) |
| Failures | 0 |
| Windows decoded | 155 |
| Speech audio | 15.9 min |
| Cost per window | ~12.2 s |
| Transcript segments stored | 380 |

Three of these correct round-1's projections and should be trusted over them:

1. **The corpus needs ~155 windows, not the ~20 round 1 projected.** That
   projection divided total speech by 30 seconds, but a window cannot span two
   clips, so the floor is one window per speech-bearing clip. 126 of 131 speech
   clips used exactly one window. Per-window cost was right; the count was off
   7x, and with it the "~3.5 minutes for the whole project" figure.
2. **The VAD gate skips 49% of clips, not 64%.** whisper.cpp's built-in VAD at
   default parameters gates less aggressively than the tuned Silero ONNX harness
   round 1 used. That costs decode time and is the safer direction, but the
   thresholds are worth revisiting.
3. **12% of speech clips get a language tag outside the supported en/ja/zh set**
   — 12 Korean, 2 Russian, 1 Polish, 1 Spanish. Round 1 called Korean a
   hallucination marker on this corpus. A World Expo plausibly contains all of
   them, so this is unverified either way, but STT language tags are currently
   **not** domain-validated against a closed vocabulary the way the design
   requires for model output.

### Clip analysis

Cloud vision analysis is opt-in and runs from either the CLI or the desktop. It
reads proxies, never originals,
and sends a deterministic pack of JPEG frames plus an accepted speech transcript
when one is current. The OpenRouter key resolves from `OPENROUTER_API_KEY` or
`~/.openrouter.key`; it is never stored in a project catalog.

- **Frame recipe**:
  `framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1`.
  Candidates begin 400 ms after the start and omit the final 400 ms. Selection
  is deterministic and combines sharpness, brightness, motion, and dHash
  diversity into 3–8 frames.
- **Prompt version**: `vlm.observe.v2`. Every response passes a bounded JSON
  schema and domain validation. The representative thumbnail must name one of
  the supplied frame timestamps.
- **Providers**: Qwen is the default. A process-wide circuit breaker switches
  the batch to Gemini after repeated transport/provider failures. A
  domain-invalid default response is retried once with Gemini.
- **Ownership**: title, description, and AI labels are suggestions. Human notes
  are never touched; label consolidation remaps only labels recorded in the AI
  ownership side table and commits the resulting label changes as typed project
  operations. Title and description are human-editable through the dedicated
  `set_clip_summary` operation; they are deliberately not part of `AssetPatch`,
  whose four fields are a full-replacement contract shared with the CLI.
- **Staleness**: analysis is current only for the same frame recipe, prompt
  version, representation, and proxy fingerprint. Regenerating the proxy makes
  the observation stale.
- **Budget/concurrency**: the cost ceiling and worker count are parameters on
  `AnalysisRunOptions`, falling back to `RCDVLOG_VLM_MAX_COST_USD` and
  `RCDVLOG_VLM_WORKERS` (default 4) when unset. The legacy runtime ceiling is
  checked before each request against returned provider usage and is not yet
  cross-process safe. The durable admission/dispatch foundation is
  `analysis::accounting` plus migration 38: initialize its canonical authority
  before creating new runtime run/pilot rows, then atomically reserve the full
  provider-owned bound and commit the dispatch marker before transport. Provider
  wrappers and runtime/CLI wiring are follow-up work; do not claim concurrent
  runtime safety until they call this owner. Progress weights are the exact
  number of selected frames.
- **Cancellation**: batch progress callbacks return `ControlFlow`. Returning
  `Break` sets the same stop flag the budget ceiling uses, so workers stop
  claiming new clips without abandoning a request already paid for. Every stage
  commits per clip and skips current clips, so a cancelled run resumes.
- **Run accounting**: `analysis_runs` records one row per desktop run with its
  stage, counts, spend, and stop reason. The pre-run cost projection comes from
  this project's own observed cost per clip, falling back to
  `REFERENCE_COST_PER_CLIP_USD` until the first run. Like `analysis_labels`, the
  table is local and unsynced.

Within-clip moment discovery lives in `analysis/moments` and is exposed through
the thin `moments plan/discover/list/get` CLI adapter. It requires explicit
project/asset scope, provider, model, payload and spend ceiling, reuses current
proxies and accepted timed speech, and stores resumable steps and evidence in
SQLite. Reuse the shared representation claims and analysis-run accounting;
unknown usage blocks new paid requests until explicit recovery authorization.
Sparse coverage and candidate ranges remain suggestions requiring manual
review; discovery never changes inclusion, human annotations or chronology.

The `prepare preflight/run/evidence` CLI composes existing representation,
transcript, orientation, and moment evidence for an explicit project/asset
scope. Preflight and evidence require an existing explicit catalog and open it
read-only; only `prepare run` mutates, and paid moments work requires its
explicit provider/model/payload/budget policy. Pagination outcomes describe
only the current page, scope fingerprints are selectors rather than snapshots,
and sparse samples never certify arbitrary edit ranges. Full proxy replacement
invalidates transcript binding. Preparation preserves originals, human
annotations, orientation choices, and chronology; see `docs/agent-cli.md`.

Paid analysis is admitted only through a concrete `RunPolicy` (see
`docs/analysis-preflight.md`). The policy binds exact project-owned asset/source
identities, provider/model allowlist and permitted fallbacks, frames/audio/
transcript payload, explicitly approved project-vocabulary context, expected
cost, requested ceiling, prompt/schema/recipe/tool versions and readiness.
Authorization is a separate SQLite record from the bounded runtime pilot. An
explicit CLI/desktop batch invocation materializes that policy automatically,
records approval, runs the pilot, then executes the exact authorized scope; no
sample or budget approval broadens model/payload/scope. Frames-only requests
must not read or transmit transcripts; project vocabulary requires its own
explicitly approved context and stays frozen to that snapshot during a run. Pilot spend counts against
the same ceiling; unknown or interrupted usage is retained and fail-closed.
Any policy/source/schema/context change invalidates the approval and pilot,
including after restart. Provider diagnostics persist only stable categories,
never response bodies or raw error strings.

**Measured end to end**, 2026-08-04, M3 MacBook Air, four workers, 273-clip
Osaka/Expo project:

| Measure | Value |
| --- | --- |
| Main pending batch | **252 clips in 10 min 29 s** |
| Final current coverage | **273 / 273** |
| Failed or rejected | 0 |
| Transcript fused | 131 clips |
| Provider fallback | 1 clip (Qwen 429 to Gemini) |
| Stored analysis cost | **$0.02732851** |
| Negative controls | 12 / 12 passed; $0.000505 |
| Consolidated vocabulary | 6 labels; 149 asset mappings |

### Batch estimation

All long batch work should use `core/batch-estimator`, not an ad hoc ETA. Callers provide work weights, explicit worker count, optional persisted `RateHistory`, and successful per-worker elapsed observations. The estimator balances remaining weights across workers and reports the slowest shard. Failures advance item completion but do not contaminate throughput.

Current task keys are versioned. macOS proxy generation has its own history
because VideoToolbox throughput is not comparable to the server/software path:

```text
proxy.h264-720p.orientation-v3.macos-videotoolbox-w{1,2,3,4}-v2
proxy.h264-720p.orientation-v3.macos-software-w{1,2,3,4}-v2
proxy.h264-720p.orientation-v3.linux-vaapi-qp23-wN-v1
proxy.h264-720p.orientation-v3.linux-software-wN-v1
thumbnail.q1.orientation-v2
stt.whisper-large-v3-turbo-q5_0.vad-silero-v5.win30.v1-w1
vlm.observe-v2.framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1-w{1,2,3,4}-v1
```

Change the task key when settings or concurrency change enough that old throughput is no longer comparable. Proxy and thumbnail weights are source bytes. macOS proxy batches use up to four workers, single-proxy calls and thumbnails use one, and the non-macOS/server proxy path remains one worker. A future cloud/VLM task must pass its real shard concurrency.

The server persists histories in its control catalog scoped to its stable node and task key. The default resource model is one shared local-heavy slot (proxy/transcode and full-hash cannot overlap) plus up to two overlapping lightweight workers. Durable progress must supply the exact remaining weights and actual worker count; only successful item observations update throughput.

### Host calibration and the shared local-heavy slot

`rcdvlog calibrate run` is an explicit, bounded, opt-in benchmark. It uses generated synthetic media by default; `--media` is required to authorize one representative file, whose bytes are read but whose path/filename are never persisted. Readiness, preflight, import, and normal analysis must never start a benchmark or download a model. Calibration results live in canonical SQLite and are reused only when host, build, media-tool executable/version, device/model, recipe, representation kind, and candidate configuration identities all match. Running/interrupted rows, failures, not-ready model conditions, changed tools, and explicit overrides remain uncertainty and must not be turned into a calibrated ETA or a completion promise.

One progress/eta range rule applies: a single timing sample widens the reported range rather than pretending a point estimate is precise. Speech calibration compares finite, non-overlapping, in-range detected regions against the baseline before adopting a faster VAD thread setting; it never assumes one thread is universally optimal. Proxy calibration uses the existing output validation/render/audio contract and records the actual selected encoder. Explicit encoder/worker/VAD/decode overrides are validated, carry explicit provenance, and use settings-qualified history keys when they differ from the defaults.

The accepted #53 resource model is one shared local-heavy slot across proxy/transcode, full-hash, speech, and future local VLM/analysis work. The server scheduler claims the union of heavy classes from one durable eligible-work queue (newest explicit heavy work first, intentional LIFO), and lightweight work may overlap under its own cap. Proxy, transcription, preparation, and calibration entrypoints take one advisory same-host/same-user filesystem lease; batch internals must not nested-acquire it. This lock coordinates processes on one host only and is not a distributed cluster scheduler. Existing per-asset representation claims remain separate integrity locks.

### Edit planning and export

Edit-plan revisions are separate from labels and reversible removal. The basic
desktop action seeds a plan from visible, non-removed clips, while the core owns
ordered source ranges, validation, preview generation, and OTIO export. Do not
move rough-cut inclusion or source-range controls back into the labeling phase;
finish them in the dedicated edit-plan surface. Removal continues to control
labeling visibility only.

## 10. Desktop UI conventions

The UI is intentionally keyboard-first and dense, using one main `App.tsx` today. Keep domain logic out of it even if component extraction is useful.

### API and state

- `apps/desktop/src/api.ts` selects real Tauri calls when `__TAURI_INTERNALS__` exists and mock behavior otherwise.
- Optimistic asset updates call the core and replace the returned canonical `Asset`; failures roll back and surface an error.
- Progress events are `proxy-progress`, `thumbnail-progress`, `analysis-progress`, and `model-install-progress`. Each of the first three carries an optional completed asset plus a batch estimate. Merge a completed asset immediately so thumbnails, proxies, and generated titles appear as they finish. `analysis-progress` additionally carries the current stage and the run's spend so far.
- Media playback prefers an available proxy, otherwise an available original. Thumbnails are also used as the video's poster frame.
- `api.mediaUrl` builds a URL against the loopback media origin, passing the asset id and the representation it chose (`rep=proxy` or `rep=original`) plus the session token. The base URL and token arrive on `mediaOrigin`, returned by `desktopSettings` and the other settings commands and held in memory only. Keep the proxy-versus-original decision here, next to rotation and poster agreement, rather than moving it into the origin.
- When a mounted element fires `error`, the shell calls `api.recoverMediaOrigin()`, which health-checks the origin and restarts it once per session. A healthy origin is returned unchanged so an ordinary decode failure does not invalidate every URL on screen.
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
- `P`: selected proxy; Shift+`P`: missing proxies; `T`: missing thumbnails; `A`: analyze project.
- `M`: add media.
- `[`/`]`: story-ribbon zoom; `C`: center selected clip.
- `O`: rotate the selected clip 90° clockwise; Shift+`O`: flip it 180°.
- Command+`N`: new project; Command+`O`: open catalog; Command+`,`: global settings; `?`: keyboard map.

Global shortcuts must return early for text inputs, textareas, and selects, except modal-level Escape handling. Any shortcut that moves focus must call `preventDefault()` before focusing the destination.

### Story-ribbon context

The ribbon already accepts optional TypeScript fields `phase`, `location`, `timezone`, and `storyBeat` and groups contiguous equal contexts. These fields are populated only in the browser demo. When persisted context is added, prefer a core-derived per-asset display projection from explicit time/range records rather than making the optional UI fields an independent source of truth.

### CSS and layout

The app has a minimum 980×680 window and a three-column workbench. Review queue and inspector scroll independently; the player and story ribbon occupy the center. Preserve visible focus styles, semantic labels, `aria-keyshortcuts`, and reduced-clutter dark editing-bench aesthetic. Test at the configured 1440×900 default and the 980×680 minimum.

The top bar is one row that degrades rather than clips (#75). Its ladder is in
`styles.css` under "The degradation ladder": stage 1 drops the wordmark word,
the full progress form, and the lifecycle labels (into `•••`); stage 2 drops
the mode switcher's words to glyphs and the `?` button into the same menu. The
`N/N` count, Stop/Cancel, Add media, and the project switcher never give way
and are asserted whole and clickable in `e2e/topbar-degradation.spec.ts`. Each
stage is a container query on the bar with a wider twin for
`[data-operation="active"]`, because a running operation claims ~400px of the
same row. Two rules keep it working: anything added to the bar must be bounded
in width — the operation slot's compact form relocates its variable text to a
tooltip for exactly that reason — and the bar clips across but not down
(`overflow-x: clip`), because its own dropdown menus open below it.

Review video is **not** served over the Tauri asset protocol. `WKWebView` never
produces a video frame for media delivered through a custom scheme, so an
un-played clip has no first frame; the app binds its own loopback HTTP origin at
startup and `api.mediaUrl` points at that instead. See
`apps/desktop/src-tauri/src/media_server.rs`, `docs/tasks/desktop/MEDIA_DELIVERY.md`,
and the evidence in `docs/desktop/HARD_LESSONS.md`. Its eight security
requirements — loopback bind, session token, asset ids rather than paths,
storage-root re-validation, `Host`/`Origin` checks, `GET`/`HEAD` only, no token
or path in logs, stop with the app — are acceptance criteria with tests, not
prose. Do not weaken one to make a surface easier to build, and never widen the
bind address.

Thumbnails and posters still use the asset protocol, which permits app-data
cache paths, `$HOME/**`, `/Volumes/**`, and temporary RCD VlogCutter cache
fixtures. If media paths outside these scopes are introduced, update both that
scope and the loopback origin's storage roots deliberately, validate on macOS,
and do not disable security globally as a shortcut.

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
| Rust core | `cargo test -p rcdvlog-core` | `cargo test --workspace` |
| ETA library | `cargo test -p batch-estimator` | `cargo test --workspace` |
| CLI parsing/JSON | `cargo test -p rcdvlog-cli` | `pnpm test:integration` |
| Tauri commands/settings | `cargo test -p rcdvlog-desktop` | real `pnpm dev` smoke test when native behavior changed |
| Server core/API | `pnpm test:server` | two-node process test plus target-topology check when external behavior changed |
| Chrome web | `pnpm test:web` | `pnpm test:web:e2e` plus real HTTPS visual/interaction check |
| OpenAPI/deployment/docs | `pnpm check:openapi` | `pnpm check:deployment`, `pnpm check:docs`, and Compose render when available |
| Repository scripts in `scripts/` | `pnpm test:scripts` | `pnpm check:docs` |
| Formatting/lints | `cargo fmt --all -- --check` | `cargo clippy --workspace --all-targets -- -D warnings` |
| Full repository | — | `pnpm check` |

`pnpm check` runs TypeScript checking followed by all Rust workspace tests. The process-level integration suite creates a real one-second FFmpeg fixture and validates CLI persistence, WAL reads, ingest, metadata, deduplication, labels/notes/removal, clock correction, thumbnails/proxies, and offline-original review. Its media portion skips when FFmpeg or ffprobe is missing.

Use `pnpm gate --scope fast` for a quick iteration check. The default local
gate requires a JSON focused-check plan and runs the candidate diff checks,
pinned formatter/toolchain, workspace Clippy, frontend typecheck, and the
named checks in that plan. For example, save
`{"checks":[{"name":"migration test","command":"pnpm","args":["test:focused","rcdvlog-core","migrations::tests::the_schema_version_constant_tracks_the_migration_list"]}]}`
to `/absolute/path/focused-checks.json`, then run
`pnpm gate --checks /absolute/path/focused-checks.json`. Commands are spawned directly
with argument arrays; shell strings are not accepted. Local verification should
cover the changed behavior and its dependencies, including targeted integration
checks for migrations, integrity, concurrency, cancellation, or paid accounting
when those boundaries change. Use the full pre-PR scope only when a specific
risk or repeated CI failure justifies its broader cost:
`pnpm gate --scope pre-pr`. It follows all Linux CI checks in
`.github/workflows/ci.yml`, prints each command's output and status, and fails
if any prerequisite or command fails. `pnpm gate --scope fast` remains
iteration-only. Before merge, completed Linux and platform-relevant macOS CI
must pass on the exact candidate head; coverage and safety checks remain mandatory. The gate's
isolated regression suite is `node --test scripts/pre-pr-gate.test.mjs`, and
`pnpm test:scripts` includes it. Re-run relevant checks after each code change
and include results or exact-head CI links and failed-step logs in a handoff.

For one Rust test during iteration, run
`pnpm test:focused <cargo-package> <exact-test-name>` (for example,
`pnpm test:focused rcdvlog-core migrations::tests::the_schema_version_constant_tracks_the_migration_list`). It uses the CI Rust
pin and `--exact`, then requires Cargo to report the named test and at least
one executed test. The fake-command regression cases are in
`scripts/run-focused-test.test.mjs`.

Use `pnpm publish:after-gate push --checks /absolute/path/focused-checks.json` or
`pnpm publish:after-gate pr --checks /absolute/path/focused-checks.json` to publish after
the focused local gate. Choose the broader policy explicitly with
`--scope pre-pr`; the fast scope is refused at the publication boundary. The
wrapper runs the actual gate from a clean tree, snapshots HEAD and tracked/untracked
working-tree content, and rechecks that identity before each external action.
It refuses publication if the gate fails or that identity changes. The `pr`
action pushes the current branch upstream and then creates the PR, stopping if
the push fails.

After publication, inspect CI on meaningful status transitions and wait for
jobs to finish before retrieving failure logs. Repair failures locally, rerun
the gate on the repaired code, and push the new commit. Confirm CI passes on
that exact commit before declaring an issue complete; neither an in-progress
run nor a green run for an older commit is confirmation.

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

Scale acceptance runs against a large real corpus that is **not** named in this
repository. Point the tooling at it through local configuration:

```bash
export RCDVLOG_ACCEPTANCE_MEDIA_ROOT="/absolute/path/to/your/large/corpus"
```

Everything below assumes that variable. Never hard-code the path in Git, in an
issue, or in a commit message: it names somebody's private footage, and the
technical value of an acceptance run is in the counts and the behaviour, not in
where the files live.

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
2. Update the TypeScript mirror and the harness settings fixture (`apps/desktop/src/test/harness/state.ts`), which the browser preview reads too.
3. Persist atomically where practical and test a missing/older/corrupt settings file.
4. Expose the control in global settings, not per-project UI, unless the setting truly belongs in the catalog.

### Change media generation settings

1. Treat outputs as a new representation version.
2. Change cache/task keys so stale files and throughput history are not silently reused.
3. Update docs, tool checks, tests, and offline fallback behavior.
4. Test representative Sony/DJI/iPhone/Insta360 H.264/H.265 media, not only synthetic MPEG-4 fixtures.

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
