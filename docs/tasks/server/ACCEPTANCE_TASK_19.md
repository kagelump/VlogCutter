# Task 19 acceptance report

**Acceptance date:** 2026-07-28 (Asia/Tokyo)  
**Local result:** Automated and macOS local-process acceptance passed.  
**Release-topology result:** Not accepted yet. Ubuntu, Docker Compose rendering, LAN/Tailscale HTTPS, NAS path/outage behavior, native Tauri smoke, and target-laptop resource observations were unavailable and remain explicit release risks.

This is the evidence base for Task 20. A check is marked passed only when it ran in this environment; the optional real Expo source fixture was not accessed.

## Frozen compatibility and environment

- Application/workspace version: `0.1.0`.
- Sync protocol: `1.0`; baseline format: `1`.
- Project catalog schema: `8` (`peer_operation_acknowledgements` is migration 8).
- Server control catalog schema: `10`; backup format: `1`.
- Host: Apple Silicon, macOS 14.8.5 (23J423), Darwin 23.6.0.
- Rust/Cargo: 1.97.1; bundled Node: 24.14.0; available pnpm wrapper: 11.9.0.
- FFmpeg/ffprobe: 8.1.2 from `/opt/homebrew/bin`; ExifTool: 13.36 from `/usr/local/bin`.
- Browser: Google Chrome 150.0.7871.187 through Playwright 1.55-compatible workspace tooling.
- Docker: unavailable (`docker: command not found`).

The package-manager wrapper attempted a non-interactive dependency-store reconciliation and aborted before running scripts. No dependency install was required; the final frontend evidence used the already-installed workspace binaries with the bundled Node executable.

## Final automated evidence

| Boundary | Command or checked-in equivalent | Result |
| --- | --- | --- |
| Rust formatting | `cargo fmt --all -- --check` | Pass |
| Rust tests with real media tools | `PATH="/opt/homebrew/bin:/usr/local/bin:$PATH" cargo test --workspace` | Pass; all workspace suites passed, with only the pre-existing interactive macOS Keychain round-trip ignored |
| Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` | Pass |
| CLI process/media integration | Included in the workspace gate (`cli_workflow`, four tests) | Pass, including real FFmpeg ingest/thumbnail/proxy/offline-original workflow |
| Independent two-node process | `cargo test -p rcdvlog-server --test two_node_process -- --nocapture`, run with loopback permission | Pass in 5.34 s; separate Mac/server SQLite files, real HTTP auth/baseline/reconcile, duplicate delivery, browser-origin mutation, and server restart |
| Desktop frontend | Direct equivalents of `pnpm --dir apps/desktop check` and `build` | Pass; TypeScript and Vite production build |
| Shared TypeScript packages | Direct TypeScript checks for `project-client` and `review-ui` | Pass |
| Web unit/build | TypeScript, Vitest, and Vite direct equivalents | Pass; 13 tests and production build |
| Chrome E2E | Playwright against a real local server process | Pass; four tests in 26.6 s, including local-only bootstrap login, admin mutation, two-tab concurrent notes, and polling convergence with one WebSocket disabled |
| OpenAPI drift | `node scripts/sync-openapi.mjs --check` | Pass |
| Deployment invariants | `node scripts/check-server-deployment.mjs` | Pass: loopback backend, host networking, read-only container filesystem, read-only originals mount, no published backend ports, no Funnel command |
| Compose render | `docker compose -f deploy/server/compose.yaml config --quiet` | Not run; Docker is not installed on this Mac |
| Secret-pattern source scan | `git grep` for assigned server tokens, literal long Bearer credentials, and session-cookie values | Pass; no credential value found. The only `token=` URL match is an endpoint-rejection unit fixture |

All automated media and database fixtures used temporary directories. No original media, project catalog, or generated representation outside a temporary fixture was changed.

## Scale and fault evidence

- A 2,000-asset project baseline imports through the production baseline path and pages exhaustively in pages of at most 200. The focused test body completed in 9.35 s on this Mac.
- A 2,000-row global library fixture returns a bounded 50-item page without loading the archive into the API response.
- A 2,000-row scan-history fixture pages without duplicate rows.
- A 10,000-job queue preserves indexed atomic claims; the default proxy and full-hash worker caps remain one each.
- Sync origin-preserving delivery interleavings, duplicate pages, sequence gaps, remote future-clock rejection, HLC convergence, note/label/LWW/clock semantics, process restart, and lost acknowledgement behavior pass automated tests.
- Restore/resync now replays only exact contiguous server-origin operations retained by the Mac. A backup taken before a server operation was restored, the retained operation was reconciled, recovery completed, and the next server operation used sequence 2 rather than reusing sequence 1.
- Explicit per-peer acknowledgement cursors and 500-operation upload pages prevent server-origin activity from suppressing unsent Mac-origin edits.
- Job tests cover expired/stale leases, retry, cancellation, duplicate requests, crash recovery, atomic completion, and 10,000 queued definitions.
- Generated-media tests cover scan/probe/hash/proxy/thumbnail, quick/full identity convergence, symlink escape, immutable originals, missing mounts, restart reconciliation, cancellation cleanup, spaces, and non-ASCII paths.
- Range tests cover HEAD, single/open/suffix/unsatisfiable ranges without exposing filesystem paths. Whole-file server buffering is not used by the streaming implementation.

No target-machine RSS, CPU, temperature, or thermal-throttling measurements were available. The scale results establish bounded query/page behavior, not production NAS throughput.

## Security disposition

No unresolved high-impact finding remains inside the locally exercised boundary. Task 19 found and fixed these issues:

1. Remote unauthenticated bootstrap creation was removed. Bootstrap codes are now created only by the server-local admin command and written to a new owner-only file.
2. Host, Origin, forwarded-header, and WebSocket-origin validation were added. Foreign-origin WebSocket handshakes are rejected.
3. Open credential exchanges have a 64 KiB body limit and bounded in-process attempt window. The limiter resets on server restart; the private LAN/tailnet boundary remains part of the defense.
4. Security headers now include CSP, same-origin policy, and a restrictive permissions policy.
5. A browser CSRF value is shared across same-origin tabs while the session credential remains an HttpOnly cookie. It is not placed in URLs, logs, process arguments, or markup.
6. Sync responses are applied by operation origin, and authenticated server acknowledgements are stored per peer. This fixed a case where legitimate offline Mac edits could be skipped.

Existing automated coverage also passes for unauthenticated/revoked sessions and devices, bootstrap/pairing replay and expiry, CSRF mismatch, path traversal and symlink escape, arbitrary representation IDs, malformed ranges, redacted config/debug/diagnostic bundles, safe job payloads, and owner-only credential files.

Residual security limitations:

- The fixed-window authentication limiter is process-local and not intended as an internet-facing abuse-control system.
- `.ts.net` hostnames are accepted as the supported tailnet front door; authentication and Tailscale ACLs must still be configured correctly.
- Caddy and Tailscale proxy behavior was not exercised on the target network, so forwarded-host/proto interoperability remains an external acceptance item.

## Manual target-topology matrix

| # | Check | Status, reason, and risk |
| --- | --- | --- |
| 1 | Install/start documented Compose on Ubuntu i7-8550U/16 GB | Not run: no Ubuntu host or Docker. Risk: image/platform, permissions, restart policy, and host-network behavior are unverified on target. |
| 2 | Prove live SQLite is on local SSD | Not run on target. Local temporary tests keep SQLite outside source media, but the target volume mapping is unverified. |
| 3 | Mount small NAS source read-only and record protocol/options | Not run: no authorized target NAS mount. Risk: NFS/SMB permissions and metadata semantics remain unknown. |
| 4 | Bind the same logical root from a different macOS path | Automated logical-root/full-and-partial-binding tests pass; physical Mac/Linux mounts were not available. |
| 5 | Scan small NAS scope and record exact counts | Not run on NAS. Generated temporary-media scans pass; NAS latency and permission failures remain unmeasured. |
| 6 | Generate proxy/thumbnail and record elapsed/CPU/memory/thermal data | Generated-media jobs pass locally. Target worker timing, RSS, temperature, and throttling were not measured. |
| 7 | Pair disposable desktop project and reconcile offline edits | Real independent catalogs/process/HTTP pass; native Tauri pairing UI and physical two-machine offline/reconnect were not run. |
| 8 | Add server-library asset in Chrome while Mac is offline | Core/server add-to-project and sync tests pass; physical offline Mac reconnect was not run. |
| 9 | Play/seek proxy in Chrome over LAN HTTPS | Range streaming is automated; LAN HTTPS playback/seek was not run. Risk includes certificate trust and proxy buffering. |
| 10 | Repeat Chrome/desktop over Tailscale away from LAN | Not run: no tailnet endpoint/ACL context. Remote route and certificate behavior remain unverified. |
| 11 | Download proxy to Mac and review with NAS unavailable | Download/cache logic is automated; native asset-protocol playback with physical NAS loss was not run. |
| 12 | Queue and later fulfill Mac-only representation request | Job surfacing/completion is automated; physical reconnect fulfillment was not run. |
| 13 | Disconnect/reconnect NAS during scan/hash/proxy | Missing-root and job recovery tests pass; physical mount interruption timing was not run. |
| 14 | Reboot server during queued/running work | Process restart and lease recovery are automated; Ubuntu reboot/container restart was not run. |
| 15 | Revoke browser and desktop credentials | HTTP/core revocation tests pass; native desktop UI response and target browser session were not run. |
| 16 | Backup and restore to disposable alternate root | Pass in automated tests, including verification, tamper rejection, representation revalidation, and ahead-peer sequence recovery. Not repeated in Compose. |
| 17 | Prove original-root tree/metadata unchanged | Temporary generated-original immutability tests pass. The real NAS/Expo fixture was not accessed, so no real-tree before/after capture exists. |

## Release limitations and next action

Task 19 is complete as an implementation and local automated-acceptance task, but this report is not approval to deploy the companion server as release-ready. Before release, run the 17-row matrix on the actual Ubuntu/Mac/NAS/LAN/Tailscale topology, attach exact counts and resource observations to a follow-up acceptance record, and resolve any target-only defect. The known `/Volumes/data/VideoProjects/2025-08 Expo` source was deliberately left untouched.
