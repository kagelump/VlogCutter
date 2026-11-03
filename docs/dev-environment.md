# Development environment and running the app

Relocated verbatim from the repository-root `AGENTS.md` sections 6 ("Toolchain and prerequisites") and 7 ("Running the app") so the root file can stay a short index (issue #260). Rules that must not be violated (fixed ports, debug-only seams, media-origin security) are repeated inline in `AGENTS.md`.

## Toolchain and prerequisites

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

## Running the app

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
