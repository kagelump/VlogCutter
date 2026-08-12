# Dev-only eval bridge — drive the running webview from a shell

**Status** implemented. **Depends on** nothing. **Ships** never — dev builds only.

## The problem

Diagnosing anything that only manifests inside the running `WKWebView` costs a
round trip through a human for every single observation. The investigation
recorded in `docs/desktop/HARD_LESSONS.md` took roughly a dozen such round
trips: each hypothesis needed one console snippet pasted in and one JSON blob
pasted back, and several of those exchanges existed only to discover that the
snippet needed a small change.

The failure being chased was invisible to every offline tool. It did not
reproduce in a headless harness, in Chromium, or in a faithful reimplementation
of the transport. It reproduced only in the app, and the decisive measurement —
a canvas readback returning alpha 0 — is three lines of JavaScript that must run
in that webview against that element.

Frontend tests do not cover this ground either: `api.ts` falls back to demo
fixtures outside Tauri, so anything touching real media, real catalogs or real
platform behaviour is invisible to `vitest`.

## Proposal

A small WebSocket server inside the Tauri app, active in dev builds only, that
accepts JavaScript, evaluates it in the main webview, and returns the result as
JSON. An agent or a developer drives it from a shell:

```bash
rcdvlog-eval 'document.querySelectorAll("video[data-asset-id]").length'
```

- Bind `127.0.0.1:0` and **publish the assigned address**, so a leaked previous
  instance or a second dev app cannot take the port this one needs. Discovery is
  one line of stdout for a harness that spawned the app, and an address file for
  a client that did not.
- Evaluate through Tauri's existing webview eval, resolving the completion
  value; serialise with `JSON.stringify` and return errors as
  `{error: {name, message, stack}}` rather than closing the socket.
- Support a `--file` form for multi-line scripts, since the useful probes are
  rarely one-liners.
- Log every evaluated script to stdout, so a dev session leaves a readable
  trail of what was run.

The implementation is isolated in the debug-only Tauri module
`apps/desktop/src-tauri/src/eval_bridge.rs`, with the shell-facing client at
`scripts/rcdvlog-eval`.

## Usage

Start the native desktop app with the explicit opt-in:

```bash
RCDVLOG_EVAL_BRIDGE=1 pnpm dev
```

From another shell, evaluate one expression:

```bash
pnpm eval -- 'document.querySelectorAll("video[data-asset-id]").length'
```

For a multi-line probe, put the JavaScript in a file:

```bash
pnpm eval -- --file /absolute/path/to/probe.js
```

The client prints one JSON value. JavaScript failures print
`{"error":{"name":...,"message":...,"stack":...}}` and exit non-zero. The
listener stays available after an evaluation error. The bridge refuses every
WebSocket handshake that has an `Origin` header; the repository client
deliberately sends none.

## Finding the listener

The bridge binds `127.0.0.1:0`, so nothing may assume an address. It advertises
the one it got twice, because its two clients differ:

1. **Stdout**, for a harness that spawned the app:
   `[RCDVLOG_EVAL_BRIDGE_COMPILED] listening on ws://127.0.0.1:<port>`. Parse
   this line under a startup timeout — a bridge that never comes up must fail in
   the harness, where it can be reported, not hang there.
   `scripts/lib/eval-bridge.mjs` exports `parseMarkerLine` and
   `readMarkerAddress` so the runner and the shell client cannot drift apart.
2. **An address file**, for a client started in another shell, holding
   `{"address","port","pid"}`. It defaults to `rcdvlog-eval-bridge.json` in the
   platform temp directory. `pnpm eval` reads it with no arguments, checks the
   recorded pid, and says the address is *stale* rather than connecting to a
   port that has since been handed to something else. The app removes its own
   file on exit.

| Variable | Effect |
| --- | --- |
| `RCDVLOG_EVAL_BRIDGE_PORT` | Pins the listener to one port, for a tool that can read neither channel. A value that is not a port is an error, not a fall back to ephemeral. |
| `RCDVLOG_EVAL_BRIDGE_ADDRESS_FILE` | Moves the address file, so concurrent runs never read each other's port. |

The client takes `--port <port>` to skip discovery, and `--wait <seconds>` to
poll for the address file when it is started alongside the app:

```bash
pnpm eval -- --wait 20 'document.readyState'
```

A bind failure never stops the app: `lib.rs` logs it and continues launching,
the way the media origin's does. With an ephemeral port the residual failure
mode is resource exhaustion, which is worth a line on stderr and nothing more.

## Gating — this is the part that matters

The feature is a remote code execution channel into the app. It is acceptable
only because it cannot exist in a shipped binary.

1. **`#[cfg(debug_assertions)]` around the module and the startup call.** The
   bridge uses Tauri's result-bearing webview evaluator and registers no IPC
   command. It is compiled out of release builds entirely — not disabled at
   runtime by a flag, not behind a setting. Absent.
2. **Additionally require an explicit opt-in** in dev — an env var such as
   `RCDVLOG_EVAL_BRIDGE=1` — so a routine `pnpm tauri dev` does not open a
   socket unless asked.
3. **Bind `127.0.0.1` only.**
4. **Reject unexpected `Origin` headers**, so a web page cannot reach it via
   DNS rebinding while a developer has the app open.
5. **A release build must fail a test that asserts the symbol is absent.** The
   gate is the whole safety argument, so it gets a test rather than a comment.

## Acceptance criteria

- [x] `cargo build --release` produces a binary with no eval endpoint and no
  listening socket; verified by a test, not by inspection.
- [x] With the env var unset, a dev build opens no socket.
- [x] With it set, a shell round trip returns a JSON result for an expression, and
  a structured error for a throwing one, without killing the connection.
- [x] A request with a foreign `Origin` is refused.
- [x] Every evaluated script appears in the dev server's stdout.
- [x] A held port cannot abort the app's launch, and a client with no arguments
  still finds the listener.

The transport, opt-in parser, persistent connection, and origin refusal have
Rust tests. `release_eval_bridge_absent` scans the built desktop executable for
a marker emitted by the debug listener: it asserts the marker is present in a
debug binary and absent under `cargo test --release`, making the compile-time
gate executable rather than documentary.

## Non-goals

Not a replacement for Safari Web Inspector, which stays the better tool for
interactive element picking and breakpoints. Not a test harness — it observes a
running app, it does not assert. Not a production diagnostic channel: anything
needed in a shipped build belongs in the diagnostics table, not here.

## Why it is worth building

The next platform-level defect will present the same way this one did: healthy
values in every API that reports health, and the truth visible only to a probe
someone has to write while looking at the live object. The cost of that
investigation scales with the round-trip latency of getting one expression
evaluated. This collapses that to a shell command.
