# CI workflows

## `ci.yml`

Runs on every push to `main` and every pull request targeting `main`.

### `ubuntu` job (ubuntu-latest)

Proves the full headless verification gate passes on Linux:

- Rust: formatting, clippy (deny warnings), and the full workspace test suite
- TypeScript: type checking across all frontend packages, the desktop vitest
  suite (extracted by D01-T003), and the web vitest suite
- CLI integration: the process-level `cli_workflow` test, which creates a
  real FFmpeg fixture and validates persistence, WAL reads, ingest, metadata,
  deduplication, labels/notes/removal, clock correction, thumbnails/proxies,
  and offline-original review
- Documentation: OpenAPI spec freshness, link and fence validation in
  Markdown, and deployment configuration checks

### `macos` job (macos-latest)

Proves what the `ubuntu` job cannot: that the native macOS desktop bundle
compiles and that the Tauri-specific Rust crate (`rcdvlog-desktop`) passes its
own test suite. This job exists because the desktop is the product, and a gate
that never compiles it is not a gate.

The `cargo test -p rcdvlog-desktop` step runs before `tauri build` so that a
test failure surfaces faster than waiting through a full native compile.

### What CI does not prove

These items from `AGENTS.md` §12 ("do not pretend headless tests validate")
are not and cannot be proven by CI:

- DaVinci Resolve import
- macOS dialogs and filesystem permission prompts
- Tauri asset-protocol playback
- Sleep/wake or source-volume disconnect/reconnect
- Expo-scale NAS performance
- Ubuntu/Compose restart and real NFS/SMB mount behavior
- LAN Caddy trust, Tailscale Serve/ACLs, and endpoint failover
- Target-laptop CPU/RSS/thermal behavior
- Playwright web end-to-end tests (`pnpm test:web:e2e` needs browser downloads
  and a running server, and stays a local manual check)
