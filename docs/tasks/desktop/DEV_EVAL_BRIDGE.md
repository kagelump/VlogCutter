# Dev-only eval bridge — drive the running webview from a shell

**Status** proposed. **Depends on** nothing. **Ships** never — dev builds only.

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

- Bind `127.0.0.1` on a **fixed dev port**, so tooling needs no discovery step.
- Evaluate through Tauri's existing webview eval, resolving the completion
  value; serialise with `JSON.stringify` and return errors as
  `{error: {name, message, stack}}` rather than closing the socket.
- Support a `--file` form for multi-line scripts, since the useful probes are
  rarely one-liners.
- Log every evaluated script to stdout, so a dev session leaves a readable
  trail of what was run.

Roughly 60 lines of Rust plus a thin shell wrapper.

## Gating — this is the part that matters

The feature is a remote code execution channel into the app. It is acceptable
only because it cannot exist in a shipped binary.

1. **`#[cfg(debug_assertions)]` around the module, the command registration,
   and the startup call.** Compiled out of release builds entirely — not
   disabled at runtime by a flag, not behind a setting. Absent.
2. **Additionally require an explicit opt-in** in dev — an env var such as
   `RCDVLOG_EVAL_BRIDGE=1` — so a routine `pnpm tauri dev` does not open a
   socket unless asked.
3. **Bind `127.0.0.1` only.**
4. **Reject unexpected `Origin` headers**, so a web page cannot reach it via
   DNS rebinding while a developer has the app open.
5. **A release build must fail a test that asserts the symbol is absent.** The
   gate is the whole safety argument, so it gets a test rather than a comment.

## Acceptance criteria

- `cargo build --release` produces a binary with no eval endpoint and no
  listening socket; verified by a test, not by inspection.
- With the env var unset, a dev build opens no socket.
- With it set, a shell round trip returns a JSON result for an expression, and
  a structured error for a throwing one, without killing the connection.
- A request with a foreign `Origin` is refused.
- Every evaluated script appears in the dev server's stdout.

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
