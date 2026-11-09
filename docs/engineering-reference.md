# Engineering reference

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

## 9. Core data and media behavior

Full recipes and measured tables: [docs/core-media-reference.md](core-media-reference.md); read it before changing these areas. Rules not to violate are inline.

### Assets, ingest, chronology

- `Asset` is defined canonically in `core/rcdvlog-core/src/types.rs` and mirrored in `apps/desktop/src/types.ts`. Availability is computed on read: `originalAvailable` needs a node-local location whose sampled fingerprint was verified; `proxyAvailable` needs the stored proxy path to exist; thumbnails are returned only when the file exists.
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
- **A batch never downloads a model**; it fails naming the file, size, and location. Installation is explicit (global settings or CLI `models install`). Models live in `<data dir>/com.rcd.vlogcutter/models/` (the older per-project `cache/models/` is still read). CLI `models status` and `transcribe batch` semantics: [docs/agent-cli.md](agent-cli.md).

### Clip analysis (opt-in cloud)

- Reads proxies, never originals; sends a deterministic frame pack (recipe `framepack.2fps.skip400ms.max40.win3-8.sharp-bright-motion.dhash8-d5.v1`) plus an accepted transcript when current. Prompt `vlm.observe.v2` with bounded-schema and domain validation. The OpenRouter key comes from `OPENROUTER_API_KEY` or `~/.openrouter.key` and is never stored in a catalog. Qwen is the default with Gemini fallback.
- Agent-run direct Gemini native-video/Flex experiments are not a product adapter; see "Direct Gemini experiments" in [docs/native-video-review.md](native-video-review.md).
- Output is a suggestion layer: human notes are never touched; label consolidation remaps only labels in the AI ownership table and commits typed operations; title/description use `set_clip_summary`, not `AssetPatch`. Analysis is stale when frame recipe, prompt version, representation, or proxy fingerprint changes.
- Paid analysis is admitted only through a concrete `RunPolicy` ([docs/analysis.md](analysis.md)) with a separate authorization record and bounded pilot; frames-only requests never read or send transcripts, project vocabulary needs its own approval, unknown/interrupted usage is fail-closed, and any policy/source/schema/context change invalidates approval. Provider diagnostics persist stable categories only. The legacy runtime ceiling is not cross-process safe; do not claim concurrent safety until runtime wiring calls `analysis::accounting`.
- Cancellation: batch callbacks return `ControlFlow`; `Break` stops new claims without abandoning paid requests, and every stage commits per clip. `moments` and `prepare` CLI adapters take explicit scope, are suggestions requiring review, and never change inclusion, human annotations, or chronology.

### Batch estimation, calibration, publishing, edit planning

- All long batches use `core/batch-estimator` with explicit worker count and versioned task keys; change the key when settings or concurrency change. Failures never contaminate throughput history. Task keys: [docs/batch-estimation.md](batch-estimation.md).
- `rcdvlog calibrate run` is explicit and opt-in; readiness, preflight, import, and analysis never start a benchmark or download a model. One shared local-heavy slot covers proxy/transcode, full-hash, speech, and future local analysis; entrypoints take one advisory same-host filesystem lease and batch internals must not nested-acquire it.
- YouTube: scope is exactly `youtube.upload`; never add read/management scopes; never log tokens, codes, verifiers, or session URLs; never silently recreate an expired session. Callers invoke upload explicitly after a successful render ([docs/youtube-upload.md](youtube-upload.md)).
- Edit-plan revisions are separate from labels and removal; rough-cut inclusion and source ranges live only in the edit-plan surface. Removal controls labeling visibility only.

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
| Chrome web | `pnpm test:web` | `pnpm test:web:e2e` (mandatory in CI as `web-e2e`) plus real HTTPS visual/interaction check |
| OpenAPI/deployment/docs | `pnpm check:openapi` | `pnpm check:deployment`, `pnpm check:docs`, and Compose render when available |
| Repository scripts in `scripts/` | `pnpm test:scripts` | `pnpm check:docs` |
| Formatting/lints | `cargo fmt --all -- --check` | `cargo clippy --workspace --all-targets -- -D warnings` |
| Full repository | — | `pnpm check` |

CI runs the Rust workspace tests with cargo-nextest (version pinned as `NEXTEST_VERSION` in `.github/workflows/ci.yml`; install the same version locally, e.g. `cargo install --locked cargo-nextest@<version>`), sharded as `linux-rust (tests N/4)` jobs via `--partition count:N/4`, plus `cargo test --workspace --doc` because nextest does not run doctests. `.config/nextest.toml` fails a hung test by name after 5 minutes (`slow-timeout` 60 s period, `terminate-after = 5`). Nothing is skipped or retried.

`pnpm check` runs TypeScript checking followed by all Rust workspace tests. The process-level integration suite creates a real one-second FFmpeg fixture and validates CLI persistence, WAL reads, ingest, metadata, deduplication, labels/notes/removal, clock correction, thumbnails/proxies, and offline-original review. Its media portion skips when FFmpeg or ffprobe is missing.

Gate commands (full usage, JSON focused-check plan format, publication wrapper, and post-publication CI handling: [docs/gate-and-publication.md](gate-and-publication.md)):

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

Scale acceptance runs against a large real corpus that is **not** named in this repository; point tooling at it with `RCDVLOG_ACCEPTANCE_MEDIA_ROOT`. Never hard-code its path in Git, an issue, or a commit message (it names somebody's private footage). It is source media, not a disposable test directory: confirm the volume is mounted and permission granted, enumerate read-only first, never write generated files into the source tree, use a catalog under the laptop-local projects root, start with a small subset, and report exact scan/import/failure counts while preserving partial work. It is periodic manual acceptance. Checklist and coverage: [docs/acceptance-fixture.md](acceptance-fixture.md).

## 14. Change recipes

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
