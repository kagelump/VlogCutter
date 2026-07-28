# Task 18: CLI and Machine-Readable Diagnostics

**Status:** Implemented (2026-07-28)
**Depends on:** Tasks 5, 6, 8, 9, 11, and 17
**Unblocks:** Task 19

## Objective

Extend the CLI and server-local administration surface with stable JSON commands for server, library, storage, jobs, synchronization, representations, credentials, backups, and recovery.

## User outcome

The user and coding agents can inspect and operate the companion server precisely without scraping the web UI, parsing prose, opening SQLite manually, or accidentally mutating state during diagnosis.

## Scope

- Add authenticated remote server commands to the existing `rcdvlog` CLI where appropriate.
- Add local-only `rcdvlog-server admin` commands for bootstrap, backup inspection, restore, and sensitive recovery.
- Add read-only server/catalog/project/library/storage/job/sync/representation/backup diagnostics.
- Add explicitly named mutation commands for pairing codes, scans, job retry/cancel, credential revoke, backup create, and restore.
- Preserve the one-envelope stdout/stderr and stable error-code contract.
- Add pagination, limits, request IDs, protocol/version fields, and redaction.
- Add process-level JSON contract tests and read-only mutation guards.
- Update the desktop last-selected breadcrumb/diagnostic path where server/sync state affects screenshot-led debugging.

## Out of scope

- Human-formatted interactive TUI
- Direct SQLite editing
- Generic arbitrary HTTP request command
- MCP server
- Original-media deletion or cleanup commands

## Proposed command families

Exact Clap spelling is finalized during implementation, but cover:

```text
rcdvlog server status
rcdvlog server projects
rcdvlog server library list|diagnose
rcdvlog server storage list|diagnose
rcdvlog server jobs list|diagnose|retry|cancel
rcdvlog server sync status|diagnose|now
rcdvlog server representations status|request
rcdvlog server credentials list|revoke
rcdvlog server backups list|diagnose|create

rcdvlog-server admin bootstrap create
rcdvlog-server admin backup inspect
rcdvlog-server admin restore inspect|apply
rcdvlog-server admin diagnostics bundle
```

## Diagnostic separation

Every focused diagnosis separates:

- Stored canonical state
- Live filesystem/network/tool observation
- Derived availability or render decision
- Version/recipe/protocol identity
- Warnings and conflicts
- Recommended explicit mutations

A read-only diagnosis may stat or probe an already known external path when the command contract says so, but it must not migrate, scan directories broadly, hash full files, create cache output, enqueue jobs, change cursors, or repair data.

Opening a SQLite catalog "read only" still performs hot-journal recovery when a previous
writer died, which changes bytes on disk. The read-only guarantee is therefore about domain
state — schema version, operation sequences, scan generations, job rows, cursors, and cache
trees — and the commands open catalogs in a mode that cannot recover or checkpoint on behalf
of another process. Immutability tests assert domain state, and any unavoidable byte-level
exception is named explicitly rather than left to surprise a future reader.

## Concurrency with running processes

The CLI is a separate process from the desktop app and the server. Two rules follow:

- `sync now` must not create a second reconcile loop for a project the desktop app is already
  reconciling, which Task 11 forbids. It signals the running app when one holds the catalog,
  and otherwise runs a single bounded reconciliation under an exclusive lock. It never
  reconciles concurrently with the app, and it reports which path it took.
- Server-local `admin` commands operate on a data directory guarded by `<dataDir>/server.lock`.
  A command that requires exclusive access refuses to run against a live server with a clear
  error instead of opening the databases anyway.

## Non-interactive credentials

Remote commands authenticate with the paired device credential. On macOS a separate binary
reading the desktop app's Keychain item triggers an interactive approval prompt, which a
headless agent cannot satisfy. The command family therefore supports one documented
non-interactive source — an explicit credential file with owner-only permissions or a named
environment variable — validated for permissions and never echoed. Keychain remains the
default for interactive use.

## Design requirements

1. Success writes exactly one JSON envelope to stdout and exits zero.
2. Failure keeps stdout empty, writes exactly one JSON error envelope to stderr, and exits nonzero.
3. Long explicit mutations may emit documented newline-delimited progress events to stderr before final stdout success.
4. Remote auth uses the paired credential from Keychain or another documented secret-safe source; tokens never appear in process arguments or output.
5. Server-local sensitive commands verify they are operating on explicit, validated data/backup roots.
6. Pagination is explicit and stable; commands never dump an unbounded archive by default.
7. Read commands are tested for file/database immutability.
8. Error codes distinguish invalid input, not found, unauthorized, incompatible protocol, media offline, mount unavailable, conflict, retryable job failure, and internal failure. They come from one checked-in registry shared with the HTTP API so a condition has the same stable code in both surfaces, and a drift check fails when one adds a code the other does not know.
9. Request IDs connect CLI errors to server logs.
10. Diagnostic output does not expose session cookies, device tokens, bootstrap codes, or unnecessary full paths.
11. Existing CLI command names/envelopes remain compatible unless intentionally versioned and documented.
12. Agent-relevant new UI-selected entities gain stable selected IDs and focused diagnosis together.

## Likely implementation areas

- `apps/cli/src/main.rs` and command modules extracted as warranted
- `apps/cli/tests/cli_workflow.rs` and new server workflow tests
- `apps/server` admin command entry points
- `core/rcdvlog-server` diagnostic projections
- `core/rcdvlog-sync` sync diagnosis
- Desktop settings/breadcrumb fields

## Acceptance criteria

- [x] All proposed state categories have bounded read-only JSON commands.
- [x] Each explicit mutation has a distinct command name and idempotent/request semantics where applicable.
- [x] Success/error/progress streams obey the CLI envelope contract.
- [x] Authentication secrets are absent from argv, stdout, stderr, logs, and fixtures.
- [x] Read-only commands preserve domain state; static backup inspection is process-tested not to open the live data catalog.
- [x] A screenshot-led workflow can resolve selected project/asset, server pairing, sync status, node availability, and representation decision.
- [x] Library diagnosis explains quick/full fingerprint evidence and mismatches.
- [x] Job diagnosis explains state, safe lease expiry, attempts, retryability, progress, and output identity.
- [x] Sync diagnosis explains per-origin cursors, pending operations, endpoint, compatibility, observations, and conflicts.
- [x] Backup diagnosis explains coverage, integrity, retention, and restore compatibility.
- [x] Existing CLI workflow tests continue to pass.
- [x] Contract tests assert command shapes, field names, request IDs, and stable error codes.
- [x] The CLI and HTTP API resolve identical conditions through the shared checked-in error registry.
- [x] `sync now` alongside a running desktop app does not start a second reconcile loop and reports the path it took.
- [x] An `admin` command needing exclusive access refuses a live data directory instead of opening it.
- [x] Remote commands authenticate non-interactively from the documented credential source, reject world-readable credential files, and keep secrets out of argv and output.

## Verification

Run unit and process-level CLI/server tests. Snapshot representative JSON only where stable and avoid timestamps/paths that make fixtures brittle. Explicitly hash temporary databases/cache trees before and after every read-only diagnostic test.

## Handoff notes

Task 20 will update `docs/agent-cli.md` only after Task 19 confirms the final command names and fields.

Implementation notes:

- Remote commands use a paired device token from `RCDVLOG_SERVER_TOKEN`, an owner-only credential file, or the interactive Keychain fallback, in that precedence order.
- `sync now` either writes a bounded wake request for the desktop-owned sync manager or performs one reconciliation while holding the project-local process lock.
- Local bootstrap secrets and remote pairing codes are written only to new `0600` files. The JSON envelope reports the file disposition but never the secret.
- Job diagnostics expose lease activity and expiry, but deliberately omit the lease owner and lease token.
