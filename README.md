# RCD VlogCutter

A local-first macOS review bench for trip footage. The MVP references originals on NAS or removable storage, keeps review proxies and thumbnails in each project folder, supports notes and fixed clock corrections, and exports a single-track hard-cut OpenTimelineIO sequence.

## Prerequisites

* Apple Silicon Mac with macOS 13 or later
* Xcode command-line tools
* Rust 1.85 or later and Cargo
* Node.js 22 or later and pnpm 10
* FFmpeg and ffprobe on `PATH`
* ExifTool on `PATH`

The current development machine uses Rust 1.97, Node 24, pnpm 10, FFmpeg 7, and ExifTool 13.

## Run in development

```bash
pnpm install
pnpm dev
```

To start directly with an existing catalog, pass the same override used by the CLI:

```bash
RCDVLOG_CATALOG=/absolute/path/catalog.sqlite3 pnpm dev
```

You can also switch catalogs at runtime with **Open catalog** in the desktop toolbar. The project selector lists every project in the active catalog.

New projects default to `~/Movies/VideoProjects/<project name>/`. Each project folder contains its own `catalog.sqlite3`, proxy cache, and exports. Use **Settings** in the desktop toolbar or **Change…** on the first-run screen to configure a different parent folder. Global settings also control autoplay: never, only when a proxy is available (the default), or always. Autoplay starts muted; the **Muted / Sound on** transport button keeps the session-wide playback state consistent as you move between clips.

## Verify

```bash
pnpm check
```

The Rust integration suite creates real fixture media with FFmpeg and verifies ingest, metadata probing, sampled deduplication, notes, review state, clock correction, proxy generation, and OTIO export.

Run only the process-level integration suite with `pnpm test:integration`. See [docs/integration-testing.md](docs/integration-testing.md) for its automated coverage and the Resolve, Tauri, and NAS acceptance checks that remain manual.

Long-running media batches use the worker-aware weighted ETA library in `core/batch-estimator`; see [docs/batch-estimation.md](docs/batch-estimation.md) for its persistence and multi-worker model.

## Agent CLI

Build the CLI:

```bash
pnpm build:cli
```

The binary is `target/debug/rcdvlog`. Pass `--catalog <project folder>/catalog.sqlite3` or set `RCDVLOG_CATALOG` to select a desktop project explicitly. Without an override it uses the legacy application-support catalog.

```bash
target/debug/rcdvlog doctor
target/debug/rcdvlog project list
target/debug/rcdvlog project status --project <project-id>
target/debug/rcdvlog clips list --project <project-id> --limit 20
```

Successful commands write one JSON envelope to stdout. Failures write a JSON error envelope to stderr and exit nonzero. Long ingest commands write JSON progress events to stderr without contaminating the final stdout response. See [docs/agent-cli.md](docs/agent-cli.md) for the full contract.

## Current MVP workflow

1. Create a project.
2. Add a folder from a mounted NAS, SSD, SD card, or local disk.
3. Generate quality-1 thumbnails and laptop proxies as needed, then review clips in datetime order.
4. Add notes and review state from the keyboard-first review bench.
5. Apply a fixed offset from a selected filename onward for one camera.
6. Mark full clips or source ranges for the rough cut.
7. Export the single-track `.otio` sequence and import it into DaVinci Resolve.

See [PRD.md](PRD.md) for the scope contract and implementation decisions.
