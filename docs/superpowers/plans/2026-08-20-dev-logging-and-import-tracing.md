# Dev Logging & Import Tracing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist all `pnpm dev` output to a gitignored, size-capped `./logs/` folder, and instrument the import flow with `tracing` so a freeze can be pinned to a specific file, stage, or the proxy concurrency step.

**Architecture:** A Node dev-wrapper (`scripts/dev-with-logs.mjs`) spawns `tauri dev` and tees combined stdout+stderr to both the terminal and a per-session file in `./logs/`, enforcing a ~100 MB FIFO cap via a pure, unit-tested prune function. The Rust backend installs a dev-only `tracing-subscriber` writing to stderr, so structured import events interleave in time order into that same session file. Import functions (desktop `run_smart_import`/`preflight_import`, core `run_import` and `generate_missing_proxies_with_progress`) gain before/after per-item and per-worker events.

**Tech Stack:** Node 20 ESM + `node:test` (scripts), Rust + `tracing` / `tracing-subscriber` (already workspace deps), Tauri 2.

## Global Constraints

- `./logs/` MUST be gitignored; never committed.
- Total size of `./logs/` capped at ~100 MB (`TOTAL_CAP_BYTES = 100 * 1024 * 1024`); per-file soft cap ~90 MB (`FILE_CAP_BYTES = 90 * 1024 * 1024`); prune oldest-first (FIFO by mtime).
- `pnpm dev` MUST remain behaviorally identical: same live terminal output, Ctrl-C stops the app, exit code is the child's.
- Rust subscriber is dev-only: active when `debug_assertions` OR env `RCDVLOG_LOG` is set; default filter `info,rcdvlog_core=debug,rcdvlog_desktop_lib=debug`; init via `try_init` so double-init never panics.
- Logging is a convenience, never a launch blocker: on any logging setup failure, warn and continue (match existing eval-bridge / media-origin error handling).
- Rust deps use workspace versions: `tracing.workspace = true`, `tracing-subscriber.workspace = true`.
- Script modules follow the existing `scripts/lib/*.mjs` + `scripts/*.test.mjs` pattern.

---

### Task 1: Prune logic (pure, tested)

**Files:**
- Create: `scripts/lib/dev-log-rotation.mjs`
- Test: `scripts/dev-log-rotation.test.mjs`

**Interfaces:**
- Produces:
  - `planPrune(files, totalCapBytes)` → `string[]` of filenames to delete. `files` is `Array<{ name: string, size: number, mtimeMs: number }>`. Deletes oldest (smallest `mtimeMs`) first until the summed `size` of the remaining files is `<= totalCapBytes`. Never deletes to get *under* if already at/under cap. Returns names in deletion order.
  - `sessionFileName(date = new Date())` → `string` like `dev-YYYYMMDD-HHMMSS.log` (zero-padded, local time).
  - `TOTAL_CAP_BYTES` (number), `FILE_CAP_BYTES` (number).

- [ ] **Step 1: Write the failing test**

```javascript
// scripts/dev-log-rotation.test.mjs
// Run with: node --test "scripts/*.test.mjs"
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  planPrune,
  sessionFileName,
  TOTAL_CAP_BYTES,
  FILE_CAP_BYTES,
} from "./lib/dev-log-rotation.mjs";

describe("planPrune", () => {
  it("deletes nothing when total is under cap", () => {
    const files = [
      { name: "a.log", size: 10, mtimeMs: 1 },
      { name: "b.log", size: 10, mtimeMs: 2 },
    ];
    assert.deepEqual(planPrune(files, 100), []);
  });

  it("deletes nothing when total is exactly at cap", () => {
    const files = [
      { name: "a.log", size: 50, mtimeMs: 1 },
      { name: "b.log", size: 50, mtimeMs: 2 },
    ];
    assert.deepEqual(planPrune(files, 100), []);
  });

  it("deletes oldest-first until remaining is under cap", () => {
    const files = [
      { name: "new.log", size: 60, mtimeMs: 3 },
      { name: "old.log", size: 60, mtimeMs: 1 },
      { name: "mid.log", size: 60, mtimeMs: 2 },
    ];
    // total 180 > 100; delete old(60)->120, mid(60)->60 <= 100. Stop.
    assert.deepEqual(planPrune(files, 100), ["old.log", "mid.log"]);
  });

  it("handles empty input", () => {
    assert.deepEqual(planPrune([], 100), []);
  });

  it("caps are the documented sizes", () => {
    assert.equal(TOTAL_CAP_BYTES, 100 * 1024 * 1024);
    assert.equal(FILE_CAP_BYTES, 90 * 1024 * 1024);
  });
});

describe("sessionFileName", () => {
  it("formats a zero-padded local timestamp", () => {
    const d = new Date(2026, 7, 20, 9, 5, 3); // 2026-08-20 09:05:03 local
    assert.equal(sessionFileName(d), "dev-20260820-090503.log");
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `node --test "scripts/dev-log-rotation.test.mjs"`
Expected: FAIL — module `./lib/dev-log-rotation.mjs` not found.

- [ ] **Step 3: Write minimal implementation**

```javascript
// scripts/lib/dev-log-rotation.mjs
// Pure helpers for the dev-with-logs wrapper. No I/O here so they stay testable.

export const TOTAL_CAP_BYTES = 100 * 1024 * 1024;
export const FILE_CAP_BYTES = 90 * 1024 * 1024;

// Oldest-first FIFO prune: return the names to delete so the remaining files
// sum to <= totalCapBytes. Already under cap => delete nothing.
export function planPrune(files, totalCapBytes) {
  const ordered = [...files].sort((a, b) => a.mtimeMs - b.mtimeMs);
  let total = ordered.reduce((sum, f) => sum + f.size, 0);
  const toDelete = [];
  for (const file of ordered) {
    if (total <= totalCapBytes) break;
    toDelete.push(file.name);
    total -= file.size;
  }
  return toDelete;
}

function pad(value, width = 2) {
  return String(value).padStart(width, "0");
}

export function sessionFileName(date = new Date()) {
  const stamp =
    `${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}` +
    `-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}`;
  return `dev-${stamp}.log`;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `node --test "scripts/dev-log-rotation.test.mjs"`
Expected: PASS (all cases).

- [ ] **Step 5: Commit**

```bash
git add scripts/lib/dev-log-rotation.mjs scripts/dev-log-rotation.test.mjs
git commit -m "feat(dev-logs): add tested FIFO prune + session-name helpers"
```

---

### Task 2: dev-with-logs wrapper + wire `pnpm dev` + gitignore

**Files:**
- Create: `scripts/dev-with-logs.mjs`
- Modify: `package.json` (`scripts.dev`)
- Modify: `.gitignore` (add `logs/`)

**Interfaces:**
- Consumes: `planPrune`, `sessionFileName`, `TOTAL_CAP_BYTES`, `FILE_CAP_BYTES` from Task 1.
- Produces: an executable script; `pnpm dev` runs it. No importable exports.

- [ ] **Step 1: Add `logs/` to .gitignore**

Append a line to `.gitignore`:

```
# Dev session logs (see scripts/dev-with-logs.mjs)
logs/
```

Verify it is ignored:

Run: `mkdir -p logs && touch logs/dev-test.log && git check-ignore logs/dev-test.log && rm logs/dev-test.log`
Expected: prints `logs/dev-test.log` (ignored).

- [ ] **Step 2: Write the wrapper script**

```javascript
// scripts/dev-with-logs.mjs
// Runs `tauri dev` and tees its combined output to ./logs/dev-<timestamp>.log,
// keeping ./logs bounded to TOTAL_CAP_BYTES (oldest-first). Logging never blocks
// the dev server: any fs failure degrades to plain passthrough.
import { spawn } from "node:child_process";
import { createWriteStream, existsSync, mkdirSync, readdirSync, rmSync, statSync } from "node:fs";
import { resolve } from "node:path";
import {
  FILE_CAP_BYTES,
  TOTAL_CAP_BYTES,
  planPrune,
  sessionFileName,
} from "./lib/dev-log-rotation.mjs";

const repoRoot = resolve(import.meta.dirname, "..");
const logsDir = resolve(repoRoot, "logs");

function listLogFiles() {
  return readdirSync(logsDir)
    .filter((name) => name.startsWith("dev-") && name.endsWith(".log"))
    .map((name) => {
      const { size, mtimeMs } = statSync(resolve(logsDir, name));
      return { name, size, mtimeMs };
    });
}

function prune() {
  for (const name of planPrune(listLogFiles(), TOTAL_CAP_BYTES)) {
    try {
      rmSync(resolve(logsDir, name));
    } catch {
      // A file we cannot delete is not worth aborting the dev server over.
    }
  }
}

// Best-effort file sink. Returns { write, close } that no-op if logging is unavailable.
function openSink() {
  try {
    if (!existsSync(logsDir)) mkdirSync(logsDir, { recursive: true });
    prune();
    let current = createWriteStream(resolve(logsDir, sessionFileName()), { flags: "a" });
    let bytes = 0;
    return {
      write(chunk) {
        try {
          current.write(chunk);
          bytes += chunk.length;
          if (bytes >= FILE_CAP_BYTES) {
            current.end();
            prune();
            current = createWriteStream(resolve(logsDir, sessionFileName()), { flags: "a" });
            bytes = 0;
          }
        } catch {
          // Ignore write errors; the terminal still shows output.
        }
      },
      close() {
        try {
          current.end();
        } catch {
          // already closed
        }
      },
    };
  } catch (error) {
    process.stderr.write(`[dev-logs] logging disabled: ${error.message}\n`);
    return { write() {}, close() {} };
  }
}

const sink = openSink();

const child = spawn("pnpm", ["--dir", "apps/desktop", "tauri", "dev"], {
  cwd: repoRoot,
  env: process.env,
  stdio: ["inherit", "pipe", "pipe"],
});

function tee(source, target) {
  source.on("data", (chunk) => {
    target.write(chunk);
    sink.write(chunk);
  });
}
tee(child.stdout, process.stdout);
tee(child.stderr, process.stderr);

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => child.kill(signal));
}

child.on("exit", (code, signal) => {
  sink.close();
  if (signal) {
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 0);
  }
});

child.on("error", (error) => {
  process.stderr.write(`[dev-logs] failed to start tauri dev: ${error.message}\n`);
  sink.close();
  process.exit(1);
});
```

- [ ] **Step 3: Point `pnpm dev` at the wrapper**

In `package.json`, change the `dev` script:

```json
"dev": "node scripts/dev-with-logs.mjs",
```

- [ ] **Step 4: Smoke-test the wrapper glue without a full app build**

The real `tauri dev` is heavy; verify the tee/prune/exit-code plumbing with a stub by temporarily pointing the spawn at a trivial command is overkill. Instead confirm the script parses and the log dir is created on a fast failure path:

Run: `node -e "import('./scripts/lib/dev-log-rotation.mjs').then(m => console.log(typeof m.planPrune))"`
Expected: prints `function`.

Run: `node --check scripts/dev-with-logs.mjs`
Expected: no output, exit 0 (syntax valid).

- [ ] **Step 5: Commit**

```bash
git add scripts/dev-with-logs.mjs package.json .gitignore
git commit -m "feat(dev-logs): tee pnpm dev output to gitignored ./logs with FIFO cap"
```

---

### Task 3: Rust dev-only tracing subscriber

**Files:**
- Modify: `apps/desktop/src-tauri/Cargo.toml` (add tracing deps)
- Modify: `apps/desktop/src-tauri/src/lib.rs` (`run()` and a small init helper)

**Interfaces:**
- Produces: `fn init_dev_logging()` (module-private in `lib.rs`) that installs a
  stderr `tracing-subscriber` when `cfg!(debug_assertions)` or env `RCDVLOG_LOG`
  is set, using `EnvFilter` (default `info,rcdvlog_core=debug,rcdvlog_desktop_lib=debug`),
  via `try_init` so it never panics. Called once at the top of `run()`.

- [ ] **Step 1: Add tracing deps to desktop Cargo.toml**

In `apps/desktop/src-tauri/Cargo.toml` `[dependencies]`, add:

```toml
tracing.workspace = true
tracing-subscriber.workspace = true
```

Run: `cargo tree -p rcdvlog-desktop -i tracing-subscriber >/dev/null 2>&1; echo done`
(Just ensures the manifest resolves; a full build happens in Step 4.)

- [ ] **Step 2: Add the init helper**

Add near the top of `apps/desktop/src-tauri/src/lib.rs` (after imports):

```rust
/// Install a stderr tracing subscriber for local development. The dev-with-logs
/// wrapper tees stderr to `./logs`, so these events land in the session file
/// alongside cargo/vite output. Never active in release unless `RCDVLOG_LOG`
/// is set; `try_init` keeps a second call (tests, re-entry) from panicking.
fn init_dev_logging() {
    let enabled = cfg!(debug_assertions) || std::env::var_os("RCDVLOG_LOG").is_some();
    if !enabled {
        return;
    }
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_env("RCDVLOG_LOG").unwrap_or_else(|_| {
        EnvFilter::new("info,rcdvlog_core=debug,rcdvlog_desktop_lib=debug")
    });
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true).with_writer(std::io::stderr))
        .try_init();
}
```

- [ ] **Step 3: Call it first thing in `run()`**

In `apps/desktop/src-tauri/src/lib.rs`, at the very start of `pub fn run()` (line ~2569), before `tauri::Builder::default()`:

```rust
pub fn run() {
    init_dev_logging();
    tauri::Builder::default()
```

- [ ] **Step 4: Build to verify it compiles**

Run: `cargo check -p rcdvlog-desktop`
Expected: compiles clean (warnings about unused `tracing` macros are fine until Task 4).

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/Cargo.toml apps/desktop/src-tauri/src/lib.rs
git commit -m "feat(dev-logs): install dev-only stderr tracing subscriber"
```

---

### Task 4: Instrument the import flow

**Files:**
- Modify: `apps/desktop/src-tauri/src/lib.rs` (`preflight_import`, `run_smart_import`)
- Modify: `core/rcdvlog-core/src/preflight.rs` (`run_import`)
- Modify: `core/rcdvlog-core/src/lib.rs` (`generate_missing_proxies_with_progress`)

**Interfaces:**
- Consumes: the subscriber from Task 3 (events are inert without it).
- Produces: no API changes — only `tracing` events. Uses `tracing::{info, debug, warn}`
  and `std::time::Instant`.

- [ ] **Step 1: Instrument the desktop import commands**

In `apps/desktop/src-tauri/src/lib.rs`, add `use tracing::info;` to the imports if not
present. In `run_smart_import`, right after `cancel.store(false, Ordering::SeqCst);`
(line ~1193):

```rust
tracing::info!(
    target: "rcdvlog_desktop_lib::import",
    project_id = %project_id,
    items = plan.items.len(),
    verify_copies,
    make_proxies,
    "run_smart_import: begin"
);
```

And immediately before returning the report at the end of `run_smart_import`, add:

```rust
tracing::info!(target: "rcdvlog_desktop_lib::import", "run_smart_import: end");
```

In `preflight_import`, after `cancel.store(false, Ordering::SeqCst);` (line ~1145):

```rust
tracing::info!(
    target: "rcdvlog_desktop_lib::import",
    project_id = %project_id,
    source_path = %source_path,
    "preflight_import: begin"
);
```

- [ ] **Step 2: Instrument core `run_import` per-item**

In `core/rcdvlog-core/src/preflight.rs`, in `run_import` after `total`/`total_bytes`
are computed (line ~1388):

```rust
tracing::info!(
    target: "rcdvlog_core::import",
    total,
    total_bytes,
    verify_copies,
    "run_import: begin copy loop"
);
```

Inside the `for (index, item) in actionable.iter().enumerate()` loop, immediately
before the `match self.import_one(` call (line ~1416):

```rust
let file_started = std::time::Instant::now();
tracing::debug!(
    target: "rcdvlog_core::import",
    index,
    total,
    file = %item.filename,
    "import_one: begin"
);
```

And immediately after the `match { ... }` block closes for that iteration (after the
trailing `on_progress(...)` at line ~1489, i.e. at the end of the loop body), add:

```rust
tracing::debug!(
    target: "rcdvlog_core::import",
    index,
    file = %item.filename,
    elapsed_ms = file_started.elapsed().as_millis() as u64,
    "import_one: end"
);
```

(Place the `end` log so it runs on every non-`break` iteration. A stall then shows as
a `begin` with no matching `end`, naming the exact file.)

- [ ] **Step 3: Instrument the proxy concurrency phase**

In `core/rcdvlog-core/src/lib.rs`, in `generate_missing_proxies_with_progress` right
after `let worker_count = pending.len().clamp(1, PROXY_WORKERS);` (line ~2882):

```rust
tracing::info!(
    target: "rcdvlog_core::proxy",
    worker_count,
    pending = task_work.len(),
    "generate_missing_proxies: begin"
);
```

Inside the worker `loop`, after an asset is popped from the queue (after the
`let Some((index, asset)) = ... pop_front() else { break; };` at line ~2916):

```rust
let proxy_started = std::time::Instant::now();
tracing::debug!(
    target: "rcdvlog_core::proxy",
    index,
    asset = %asset.id,
    "proxy worker: spawn ffmpeg"
);
```

Then locate where that worker finishes the asset (the point just before it loops back
to pop the next item — after the proxy generation call and its `outcome_sender.send`).
Add, using the same `asset` binding before it is moved into the channel (bind
`let asset_id = asset.id.clone();` at spawn time if the value is moved):

```rust
tracing::debug!(
    target: "rcdvlog_core::proxy",
    index,
    asset = %asset_id,
    elapsed_ms = proxy_started.elapsed().as_millis() as u64,
    "proxy worker: ffmpeg done"
);
```

If proxy generation returns an error in this worker, log it at `warn`:

```rust
tracing::warn!(
    target: "rcdvlog_core::proxy",
    asset = %asset_id,
    "proxy worker: ffmpeg failed"
);
```

(Read the surrounding worker body first to bind `asset_id` before the `asset` value is
moved into `outcome_sender.send(...)`, and to place the done/failed logs on the
matching arms.)

- [ ] **Step 4: Build and run the existing tests**

Run: `cargo check -p rcdvlog-core -p rcdvlog-desktop`
Expected: compiles clean.

Run: `cargo test -p rcdvlog-core --lib`
Expected: existing tests still pass (instrumentation adds no behavior).

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src-tauri/src/lib.rs core/rcdvlog-core/src/preflight.rs core/rcdvlog-core/src/lib.rs
git commit -m "feat(import): trace per-item copy and proxy-worker timing"
```

---

### Task 5: End-to-end verification & docs note

**Files:**
- Modify: `README.md` or `AGENTS.md` (one short line on `./logs` + `RCDVLOG_LOG`) — pick whichever already documents dev workflow.

- [ ] **Step 1: Run the script test suite**

Run: `pnpm test:scripts`
Expected: PASS, including the new `dev-log-rotation.test.mjs`.

- [ ] **Step 2: Verify the whole workspace builds**

Run: `cargo check --workspace`
Expected: clean.

- [ ] **Step 3: Manually verify a dev session logs**

Run: `pnpm dev` (let it boot, then Ctrl-C).
Expected: `./logs/dev-<timestamp>.log` exists and contains cargo/vite output plus a
`run` — and, if an import is triggered, `run_import: begin`/`import_one` lines.
Terminal output and Ctrl-C behavior unchanged.

- [ ] **Step 4: Document the log location**

Add one line under the dev instructions (README.md or AGENTS.md), e.g.:

```
`pnpm dev` writes a full session log to `./logs/dev-<timestamp>.log` (gitignored,
capped ~100 MB). Set `RCDVLOG_LOG=trace` to raise backend verbosity.
```

- [ ] **Step 5: Commit**

```bash
git add README.md AGENTS.md
git commit -m "docs: note ./logs dev session logging and RCDVLOG_LOG"
```

---

## Self-Review

**Spec coverage:**
- `./logs` gitignored + FIFO cap → Task 1 (prune), Task 2 (wrapper + gitignore). ✓
- `pnpm dev` unchanged behavior (tee, Ctrl-C, exit code) → Task 2. ✓
- Dev-only Rust subscriber to stderr with default filter + try_init → Task 3. ✓
- Import instrumentation (desktop cmds, core per-item before/after, proxy worker_count + per-worker) → Task 4. ✓
- Error handling: logging never blocks launch → Task 2 (openSink try/catch), Task 3 (try_init, cfg gate). ✓
- Testing: prune unit tests + build/test gates → Tasks 1, 4, 5. ✓

**Placeholder scan:** No TBD/TODO; code blocks provided for every code step. Task 4 Step 3 asks the implementer to read the worker body to bind `asset_id` correctly — this is guidance around an exact edit, not a placeholder, because the surrounding move semantics must be seen to place the log on the right arm.

**Type consistency:** `planPrune(files, totalCapBytes)`, `sessionFileName(date)`, `TOTAL_CAP_BYTES`, `FILE_CAP_BYTES`, `init_dev_logging()` used consistently across Tasks 1–3. Event targets `rcdvlog_core::import`, `rcdvlog_core::proxy`, `rcdvlog_desktop_lib::import` used consistently in Task 4.
