# Dev Logging & Import Tracing — Design

- **Date:** 2026-08-20
- **Status:** Approved
- **Author:** Ryan (with Claude)

## Problem

Importing ~198 clips from an SD card against an existing camera froze the entire
machine — the progress bar stuck at `0/198` and the OS became unresponsive until a
hard restart. There are no durable logs to diagnose what happened (suspected
resource exhaustion from copy/probe or proxy concurrency). We need (1) `pnpm dev`
to persist its output to a known, gitignored, size-capped location, and (2) enough
structured logging in the import flow to identify the last operation that ran before
the stall.

## Goals

- Running `pnpm dev` writes all of its output to `./logs/` at the repo root.
- `./logs/` is gitignored and bounded to ~100 MB total, pruned FIFO (oldest first).
- The import flow emits structured `tracing` events fine-grained enough to pin the
  freeze to a specific file, stage, or the proxy concurrency step.
- The terminal experience of `pnpm dev` is unchanged (same live output, same
  Ctrl-C behavior, same exit code).

## Non-Goals (YAGNI)

- Production / app-data-dir logging.
- A log-level toggle UI, remote log shipping, or a JSON structured sink.
- Changing import concurrency behavior. This change only *observes*; the actual fix
  (if concurrency is the cause) is a follow-up informed by the new logs.

## Approach

A Node dev-wrapper tees the full `tauri dev` output to a per-session file, and the
Rust backend routes structured `tracing` events to stderr (dev only) so they land in
that same file. One capture point, one rotation mechanism.

### 1. `./logs/` folder and gitignore

- Add `logs/` to `.gitignore`.
- Session files named `dev-YYYYMMDD-HHMMSS.log`.

### 2. `scripts/dev-with-logs.mjs`

Responsibilities:

- **Prune on start:** compute total size of `dev-*.log` files in `./logs/`; delete
  oldest (by mtime) until total ≤ the cap, so a fresh session starts under budget.
- **Open a fresh session file** `dev-<timestamp>.log`.
- **Spawn** `pnpm --dir apps/desktop tauri dev` with inherited env; pipe the child's
  stdout+stderr to BOTH the parent terminal (unchanged live view) and the file.
- **Soft per-file roll:** when the current file crosses a per-file cap, roll to a new
  `dev-<timestamp>.log` and re-prune. Keeps any single file shareable and the folder
  bounded even in a marathon session.
- **Signal + exit passthrough:** forward SIGINT/SIGTERM to the child; exit with the
  child's exit code so `pnpm dev` behaves identically to before.

Constants (module-level, easy to tweak):

- `TOTAL_CAP_BYTES` ≈ 100 MB.
- `FILE_CAP_BYTES` ≈ 90 MB.

Pure, testable core: `planPrune(files, totalCapBytes)` where `files` is
`[{ name, size, mtimeMs }]`, returning the list of filenames to delete (oldest-first
until total ≤ cap). This is unit-tested; the spawn/pipe glue is thin and left to
manual verification.

`package.json`: `"dev": "node scripts/dev-with-logs.mjs"`.

### 3. Rust tracing subscriber (dev only)

In `apps/desktop/src-tauri/src/lib.rs` `run()`, before building the Tauri app,
initialize a `tracing_subscriber` fmt layer writing to **stderr**, gated so it only
runs in dev:

- Enable when `debug_assertions` is set OR `RCDVLOG_LOG` is present.
- `EnvFilter` from `RCDVLOG_LOG` env, defaulting to
  `info,rcdvlog_core=debug,rcdvlog_desktop_lib=debug`.
- Include target and a compact timestamp; line-per-event so the tee file stays
  greppable.
- Use `try_init` (or an equivalent guard) so it never panics on double-init under
  tests or repeated `run()` calls.

Because the subscriber writes to stderr and the wrapper tees stderr, structured
events interleave in time order with cargo/vite build output, panics, and raw ffmpeg
stderr in the single session file.

### 4. Import-flow instrumentation

Add `tracing` spans/events at the seams that matter for a stall:

**Desktop (`lib.rs`):**

- `preflight_import` and `run_smart_import`: a span carrying `project_id`,
  `source_path`, `verify_copies`, `make_proxies`; log entry and exit, and each
  `DesktopImportStage` transition emitted.

**Core (`preflight.rs` `run_import`):**

- `info` at start: total actionable items, `total_bytes`, `verify_copies`.
- `debug` **before and after** each `import_one` call with filename, index, bytes,
  and elapsed. A hang then shows as the last "before" with no matching "after" —
  pinning the exact file.
- Log copy vs. verify sub-steps where cheaply available, and per-file duration.

**Core proxy phase (`lib.rs` `generate_missing_proxies_with_progress`):**

- `info` with `worker_count` (the up-to-4-way ffmpeg concurrency) and pending count.
- `debug` per asset: ffmpeg spawn start and finish with asset id + duration; log any
  spawn failure at `warn`.

Timing uses `std::time::Instant`. No heartbeat thread is needed — the before/after
per-item events already reveal a stall.

## Error Handling

- Wrapper: if `./logs/` can't be created or written, print a warning to stderr and
  still run `tauri dev` (logging is a dev convenience, never a launch blocker —
  matching how the eval-bridge and media-origin failures are handled).
- Subscriber init failure is ignored (via `try_init`); the app runs regardless.

## Testing

- `scripts/dev-with-logs.test.mjs` (runs under existing `pnpm test:scripts`): unit
  tests for `planPrune` — under cap deletes nothing, over cap deletes oldest-first
  until under cap, ties and empty inputs handled.
- Rust instrumentation: verified by running an import in `pnpm dev` and confirming
  the per-item and proxy events appear in the session log file in order. No behavior
  assertions (the change is observational).

## Rollout / Verification

1. `pnpm test:scripts` passes (prune logic).
2. `cargo check -p rcdvlog-desktop` / workspace build succeeds.
3. `pnpm dev` produces `./logs/dev-<timestamp>.log` containing build output and
   structured events; terminal output and Ctrl-C behavior unchanged.
4. Ryan reproduces the import to capture the freeze in the log.
