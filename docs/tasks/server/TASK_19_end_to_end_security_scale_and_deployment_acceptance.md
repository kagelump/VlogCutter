# Task 19: End-to-End, Security, Scale, and Deployment Acceptance

**Status:** Complete with external-topology gaps recorded in `ACCEPTANCE_TASK_19.md`
**Depends on:** Tasks 12, 15, 16, 17, and 18
**Unblocks:** Task 20

## Objective

Validate the complete companion-server system across real process, database, media, browser, network-fault, security, Ubuntu deployment, LAN, Tailscale, and NAS boundaries; fix defects found within the epic's scope.

## User outcome

The planned MVP is demonstrably safe and usable on the actual Mac/Ubuntu/NAS topology, not merely successful in unit tests or a browser mock.

## Scope

- Complete all automated workspace, integration, web, and Compose gates.
- Add missing cross-process two-node workflow coverage.
- Add sync convergence/fault-injection and restart tests.
- Add authentication, authorization, CSRF, path, range, and secret-leak security coverage.
- Add thousands-of-assets library/project scale fixtures and bounded-resource checks.
- Run real Ubuntu laptop deployment acceptance.
- Run direct LAN HTTPS and Tailscale HTTPS acceptance in Chrome and from the desktop.
- Run same-NAS/different-path root-binding acceptance.
- Run NAS disconnect/reconnect and server reboot cases.
- Run verified backup/restore drill on disposable data.
- Record exact evidence, failures, fixes, and unrun manual checks in a checked-in acceptance
  report at `docs/tasks/server/ACCEPTANCE_TASK_19.md`. Task 20 cites that path as its sole
  evidence base, so it must be a repository document rather than session output.

## Out of scope

- Expanding features to solve unrelated product gaps
- Performance tuning without measurement
- Writing generated outputs into the real source-media tree
- Resolve, edit planning, AI, multi-user, or multiple-node acceptance
- Destructive cleanup of real server or NAS data

## Automated acceptance matrix

### Schema and persistence

- Upgrade every maintained older project/server fixture.
- Reject newer/incompatible schema safely.
- Preserve data across failed migration and server restart.
- Verify read-only diagnostics remain non-mutating.

### Synchronization

- Initial baseline with thousands of assets.
- Offline operations on both Mac/server origins.
- Dropped, delayed, duplicated, and reordered network pages.
- Lost acknowledgement and process crash at commit boundaries.
- LAN/tailnet endpoint failover.
- Credential revocation and protocol incompatibility.
- Note/label/LWW/clock conflict semantics.
- Restore-then-resync: restore the server from a backup taken before operations the Mac has already committed, then reconcile and converge without sequence reuse or a wedged origin.
- Wall-clock skew and a backward clock jump on either node without poisoning HLC ordering.

### Media and jobs

- Scan/probe/hash/proxy/thumbnail using generated media.
- Quick-fingerprint collision with full-hash mismatch.
- Job restart, expired lease, retry, cancel, and duplicate request.
- Range streaming, seeking, invalid ranges, and interrupted proxy download.
- Original and derived mount outage.
- Mac-only pending representation request.

### Security

- Unauthenticated and revoked access.
- Bootstrap replay/expiry.
- CSRF, Host, Origin, forwarded-header spoofing, request-size, and rate-limit checks.
- WebSocket origin validation. Browsers do not apply CORS to WebSocket handshakes and send cookies anyway, so `/api/v1/ws/events` must reject a handshake from a foreign origin; a cross-site page must not be able to open an authenticated event stream.
- Root/path traversal, symlink escape, arbitrary-file, and malformed range attempts.
- Log, diagnostic bundle, URL, process-argument, and fixture secret scan.
- Compose assertion that original mounts are read-only and backend is not publicly bound.

### Web/desktop

- Chrome browse/add/review/annotation/admin flows.
- WebSocket loss with polling/reconciliation recovery.
- Two Chrome tabs editing the same asset concurrently, which is an ordinary single-user case and produces same-origin concurrent operations.
- Wide and narrow viewport accessibility/focus checks.
- Existing desktop keyboard, focus, playback, proxy, thumbnail, and offline behavior regression.

## Manual target-topology matrix

Run on the actual Ubuntu i7-8550U/16 GB laptop and Mac:

1. Install/start from documented Compose configuration.
2. Verify local SSD holds every live SQLite database.
3. Mount a small NAS source scope read-only using NFS if supported; record actual protocol and options.
4. Bind the same logical root from a different macOS path.
5. Scan the small scope and record scanned/imported/duplicate/failure/hash counts.
6. Generate thumbnails and proxies; record worker count, elapsed time, CPU/memory, temperature/thermal behavior, and failures.
7. Pair an existing disposable desktop project and reconcile offline edits.
8. Add a server-library asset from Chrome while the Mac is offline, then reconnect.
9. Play and seek a proxy in Chrome over LAN HTTPS.
10. Repeat Chrome and desktop connection through Tailscale away from/directly disconnected from LAN routing.
11. Download a server proxy to the Mac and review it with the NAS unavailable.
12. Queue one Mac-only representation request and fulfill it after reconnect.
13. Disconnect/reconnect NAS during scan/hash/proxy work.
14. Reboot the server during queued/running work and verify recovery.
15. Revoke browser and desktop credentials and verify behavior.
16. Create a backup and restore it to a disposable alternate data root.
17. Prove original-root contents and metadata were not changed by comparing the captured tree/evidence.

The large real corpus at `$RCDVLOG_ACCEPTANCE_MEDIA_ROOT` is optional scale acceptance. Enumerate it read-only first, begin with a small subset, never create outputs in it, and report exact counts.

## Resource expectations

- One proxy and one full-hash worker remain the default on the target server.
- Scans and paginated browsing do not grow memory proportionally to the entire library.
- Proxy streaming does not buffer the entire file in the server process.
- Desktop annotation/review remains responsive during reconciliation/download.
- Server remains responsive to health and cancellation during media work.

Record measured values rather than inventing pass thresholds after seeing results. Any release-blocking limit discovered becomes an explicit documented constraint or a scoped fix.

## Acceptance criteria

- [ ] All task-specific automated tests and final repository gates pass.
- [ ] Full two-node workflow passes with independent temporary databases and real process boundaries.
- [ ] Security matrix has no unresolved high-impact finding.
- [ ] No original fixture changes during automated or manual work.
- [ ] Sync converges after every injected recoverable network/process failure.
- [ ] Job/result state remains correct after restart/cancel/retry cases.
- [ ] Chrome playback seeking and annotation flows pass over LAN and Tailscale.
- [ ] Same logical NAS root works through different Mac/Linux paths.
- [ ] Remote-only asset and proxy-download behavior match the architecture.
- [ ] Backup verification and disposable restore succeed.
- [ ] Target server resource observations are recorded and acceptable for continued use.
- [ ] Any manual check not run is explicitly marked with reason and risk.
- [ ] `docs/tasks/server/ACCEPTANCE_TASK_19.md` records commands, versions, fixtures, counts, results, and remaining known limitations.
- [ ] A server restored from an older backup reconciles with a Mac that is ahead, without reusing an origin sequence and without wedging sync.
- [ ] A cross-origin WebSocket handshake is rejected.
- [ ] Concurrent edits from two Chrome tabs converge to one canonical state.

## Verification commands

At minimum run the final checked-in equivalents of:

```bash
pnpm --dir apps/desktop build
pnpm --dir apps/web build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm test:integration
pnpm test:web:e2e
docker compose -f deploy/server/compose.yaml config
```

## Handoff notes

Freeze implementation status, command names, config keys, protocol versions, migration versions, and known gaps in the acceptance report. Task 20 uses that evidence as its sole basis for repository-wide documentation claims.
