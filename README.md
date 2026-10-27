# RCD VlogCutter

RCD VlogCutter is a local-first, single-user footage labeling system. The macOS desktop references immutable originals on NAS or local disk, and its explicit card workflow copies from read-only removable media into a named create-only archive root. It remains fully useful offline. An optional private Ubuntu companion server adds a global media library, durable scans/media jobs, operation-based project synchronization, shared representations, Chrome review/admin, and verified backups.

The current product supports desktop project creation/opening, preflighted card import and reference-in-place folder ingest, verified media locations and archive-root rebinding, metadata probing, labels, notes, reversible removal, thumbnails, H.264 proxies, local transcription and bounded cloud clip analysis, operation-backed chronology facts and deterministic solving, durable diagnostics, versioned edit plans, low-resolution preview generation, deterministic OTIO writing, a stable JSON CLI with bounded preparation evidence commands, server project adoption/sync, browser review/annotation, storage/job administration, and recovery diagnostics. Preparation reports proxy validation, transcript provenance, sparse suggested moments, and review questions; it does not certify edit cuts or replace human review. The expanded chronology and edit-plan desktop workflows remain partial; edit-plan CLI commands, Resolve import acceptance, and the planned travel-ingest archive are not implemented.

## Prerequisites

- Desktop development: Apple Silicon Mac, macOS 13+, Xcode command-line tools
- Rust 1.85+, Node.js 22+, pnpm 10
- FFmpeg 7+ with ffprobe, and ExifTool
- Companion deployment: Ubuntu 24.04 x86-64 with Docker Engine/Compose and a host-mounted NFS (preferred) or SMB original-media share

The Task 19 acceptance environment used Rust 1.97.1, Node 24.14, FFmpeg/ffprobe 8.1.2, ExifTool 13.36, and Chrome 150. See the [acceptance report](docs/tasks/server/ACCEPTANCE_TASK_19.md) for exactly what ran and which target-topology checks remain open.

## Desktop fast start

```bash
pnpm install
pnpm dev
```

Open an existing catalog at startup with:

```bash
RCDVLOG_CATALOG=/absolute/project/catalog.sqlite3 pnpm dev
```

New projects default to `~/Movies/VideoProjects/<project-name>/`, with one local SQLite catalog and project-local cache. The projects parent and autoplay behavior are configurable in desktop Settings.

`pnpm dev` writes a full session log to `./logs/dev-<timestamp>.log` (gitignored, folder capped ~100 MB, oldest pruned first). Backend `tracing` events — including per-file import and proxy-worker timing — flow into that file; set `RCDVLOG_LOG=trace` to raise verbosity.

## Companion server fast start

The raw backend is loopback-only. Originals are mounted read-only; server state stays on local SSD; derived media and backups have separate writable roots.

```bash
cp deploy/server/example.env deploy/server/.env
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml config --quiet
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml build
docker compose --env-file deploy/server/.env -f deploy/server/compose.yaml up -d
```

Then configure trusted LAN HTTPS with Caddy or private tailnet HTTPS with Tailscale Serve. Follow the complete [companion-server guide](docs/server/README.md); do not expose the backend directly or enable Tailscale Funnel.

## Verify

```bash
pnpm check
pnpm check:openapi
pnpm check:deployment
pnpm check:docs
```

Server/web checks and manual boundaries are listed in [docs/server/development-and-testing.md](docs/server/development-and-testing.md) and [docs/integration-testing.md](docs/integration-testing.md). A passing browser mock is not evidence for native Tauri, Keychain, real NAS, LAN/Tailscale TLS, or Ubuntu behavior.

## Agent CLI

```bash
pnpm build:cli
target/debug/rcdvlog --catalog /absolute/project/catalog.sqlite3 project list
target/debug/rcdvlog server --endpoint https://<server-host>:8443 status
```

Commands emit one JSON success envelope on stdout or one JSON error envelope on stderr. Remote server commands use a paired device credential from an owner-only file, `RCDVLOG_SERVER_TOKEN`, or interactive macOS Keychain fallback; tokens never belong in argv. See [docs/agent-cli.md](docs/agent-cli.md).

Optional upload-only YouTube publishing is available through `rcdvlog youtube`.
See [OAuth setup, headless consent and resumable uploads](docs/youtube-upload.md).

## Documentation

- [Product scope and decisions](PRD.md)
- [Agent operational handoff](AGENTS.md)
- [Companion-server operations](docs/server/README.md)
- [Batch ETA semantics](docs/batch-estimation.md)
- [Integration and manual acceptance boundaries](docs/integration-testing.md)

Originals are immutable. Never move, rename, rewrite, or delete source media. The removable-card workflow only creates new verified archive copies and never overwrites them. Never copy live SQLite/WAL files between nodes.
