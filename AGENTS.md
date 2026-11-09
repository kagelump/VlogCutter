# AGENTS.md

Operational handoff for agents working on the RCD VlogCutter **source repository**. Not to be confused with the generated `AGENTS.md` planned for each user's footage project (issue #48).

## What this is

An **AI editing assistant** for travel vlogs. The goal is an end-to-end pipeline from raw clips to rough cut, where the human only gives editorial review between rough cuts. An overseeing coding agent (Codex / Claude Code) drives the work through the CLI and holds the editorial context. The app stores and maintains clip metadata as needed, but per-clip human labeling is no longer the center of the workflow.

Scale target: about 100 hours of footage across thousands of mostly 4K/60 clips from about five cameras (Sony, DJI, iPhone, Insta360). Design for that scale even when tests use tiny fixtures.

Surfaces, in priority order:

1. **Agent CLI** (`apps/cli`, binary `rcdvlog`): the primary control surface. Stable JSON in, JSON out.
2. **Companion server + Chrome web** (`apps/server`, `apps/web`): a peer to the local CLI. It handles heavy media jobs, NAS libraries, sync, backups, and editorial/TV review of rough cuts. It is optional; the local CLI and catalog work without it.
3. **Desktop app** (`apps/desktop`, Tauri/React): de-emphasized. See the footnote at the end.

## Invariants

- **Originals are immutable.** Never move, rename, rewrite, or delete source media. Generated outputs live in project-local caches or the server's derived root, never beside originals. Card import may copy into a create-only archive root and never overwrites anything.
- **SQLite is canonical.** There is one catalog per project, and the server has its own control catalog. Different machines sync typed immutable operations and never share live SQLite/WAL files. A same-machine server may attach an explicitly allow-listed CLI catalog at the exact current schema; both processes use SQLite locking and the catalog's node identity, and sync is disabled for that attached project. Do not add a competing JSON store. Generated files (e.g. workspace projections) must never become a second write path.
- **Rust owns domain logic.** `core/rcdvlog-core` (plus `rcdvlog-server` and `rcdvlog-sync`) own ingest, chronology, media, analysis, and edit planning. The CLI, HTTP handlers, and Tauri commands are thin adapters that call the same core methods.
- **Server networking:** the raw backend stays loopback-only behind Caddy or Tailscale Serve HTTPS. Never enable Funnel. Never put credentials in URLs or argv.
- **Schema changes** are forward migrations with an upgrade test. Never solve a migration by deleting a database, and never hand-edit SQLite to get product behavior; add a domain operation instead.

## Repository map

- `apps/cli`: clap JSON CLI. `apps/server`: Axum runtime and admin CLI. `apps/web`: Chrome review/admin. `apps/desktop`: Tauri app.
- `core/rcdvlog-core`: catalog, media, chronology, analysis, edit plan, OTIO. `core/rcdvlog-server`, `core/rcdvlog-sync`, `core/batch-estimator`.
- `packages/project-client`: typed browser client generated from the Rust-owned OpenAPI snapshot, which must stay in sync (`pnpm check:openapi`).
- `docs/`: focused feature docs. Start at [docs/agent-cli.md](docs/agent-cli.md), [docs/server/README.md](docs/server/README.md), and [docs/implementation-status.md](docs/implementation-status.md).

## Toolchain

Apple Silicon macOS 13+, FFmpeg 6.1+ (with libx264)/ffprobe and ExifTool on `PATH`. CI pins **Rust 1.97.1** (rustfmt, clippy), **Node 24.11.0**, **pnpm 10.17.0**, and **cargo-nextest 0.9.151**. Run `pnpm preflight` first; `pnpm gate` and CI run it too. Never silently fall back to another toolchain. If `cargo` is missing, source `~/.cargo/env` first. Details: [docs/dev-environment.md](docs/dev-environment.md).

## CLI contract

Build with `pnpm build:cli` (output: `target/debug/rcdvlog`). Always pass `--catalog /abs/path/catalog.sqlite3` in automation.

- Success: exactly one JSON envelope on stdout, exit 0. Failure: empty stdout, one JSON error envelope on stderr, nonzero exit. Progress may stream as NDJSON on stderr.
- Command names, fields, and error codes are public contracts. Never parse human prose, and add JSON contract tests when any of them change.
- Diagnostics (`doctor`, `* diagnose`, `* status`, `inspect`) are strictly read-only: no migrations or cache writes. Mutations are separate, deliberately named commands.
- Remote commands take `--endpoint` and read the device token from env, a credential file, or the Keychain, never from argv.

Full command reference: [docs/agent-cli.md](docs/agent-cli.md).

## Testing

| Change | Check |
| --- | --- |
| Rust | `cargo nextest run -p <crate>`, then `cargo nextest run --workspace` and `cargo test --workspace --doc` |
| CLI JSON | `cargo nextest run -p rcdvlog-cli`, `pnpm test:integration` |
| Server / web | `pnpm test:server`, `pnpm test:web`, `pnpm test:web:e2e` |
| Docs / OpenAPI / scripts | `pnpm check:docs`, `pnpm check:openapi`, `pnpm test:scripts` |
| Lint | `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` |

The default local gate is `pnpm gate --checks /abs/focused-checks.json`. An issue is not done until CI passes on the exact candidate head. See [docs/gate-and-publication.md](docs/gate-and-publication.md). Mocks and headless tests never prove real media, Resolve import, NAS, or Ubuntu/Caddy/Tailscale behavior. State any manual acceptance you did not run.

## Working discipline

- Run `git status` before editing and preserve uncommitted user work. Keep edits scoped, and commit each verified milestone.
- Never delete catalogs, source media, project folders, proxies, or exports unless the user explicitly asks for that exact deletion. Tests use temp dirs and never touch `~/Movies/VideoProjects` or `/Volumes`.
- Child processes use structured `Command` args, never shell strings. Preserve paths with spaces and non-ASCII names.
- Batches surface per-file failures without aborting. Long batches use `core/batch-estimator` with versioned task keys.
- The private real-footage corpus is `RCDVLOG_ACCEPTANCE_MEDIA_ROOT`. Never write its path into Git, and treat it as read-only ([docs/acceptance-fixture.md](docs/acceptance-fixture.md)).

Detailed rules (media recipes, transcription gates, CLI/test tables, change recipes): [docs/engineering-reference.md](docs/engineering-reference.md).

---

**Desktop footnote.** The Tauri desktop app still builds and ships but is no longer the primary experience. Its security seams must not be weakened: the dev eval bridge and `/debug/media` stay compiled out of release, and the loopback media origin keeps its token, `Host`, and `Origin` checks. Dev ports 1420 and 1421 are contracts. See [docs/desktop.md](docs/desktop.md).
