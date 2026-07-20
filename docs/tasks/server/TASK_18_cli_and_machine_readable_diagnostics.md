# Task 18: CLI and Machine-Readable Diagnostics

**Status:** Planned
**Depends on:** Tasks 5, 6, 8, 11, and 17
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

## Design requirements

1. Success writes exactly one JSON envelope to stdout and exits zero.
2. Failure keeps stdout empty, writes exactly one JSON error envelope to stderr, and exits nonzero.
3. Long explicit mutations may emit documented newline-delimited progress events to stderr before final stdout success.
4. Remote auth uses the paired credential from Keychain or another documented secret-safe source; tokens never appear in process arguments or output.
5. Server-local sensitive commands verify they are operating on explicit, validated data/backup roots.
6. Pagination is explicit and stable; commands never dump an unbounded archive by default.
7. Read commands are tested for file/database immutability.
8. Error codes distinguish invalid input, not found, unauthorized, incompatible protocol, media offline, mount unavailable, conflict, retryable job failure, and internal failure.
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

- [ ] All proposed state categories have bounded read-only JSON commands.
- [ ] Each explicit mutation has a distinct command name and idempotent/request semantics where applicable.
- [ ] Success/error/progress streams obey the CLI envelope contract.
- [ ] Authentication secrets are absent from argv, stdout, stderr, logs, and fixtures.
- [ ] Read-only commands leave database hashes/mtimes, operation sequences, scan generations, jobs, and cache trees unchanged.
- [ ] A screenshot-led workflow can resolve selected project/asset, server pairing, sync status, node availability, and representation decision.
- [ ] Library diagnosis explains quick/full fingerprint evidence and mismatches.
- [ ] Job diagnosis explains state, lease, attempts, retryability, progress, and output identity.
- [ ] Sync diagnosis explains per-origin cursors, pending operations, endpoint, compatibility, observations, and conflicts.
- [ ] Backup diagnosis explains last success, coverage, integrity, retention, and restore compatibility.
- [ ] Existing CLI workflow tests continue to pass.
- [ ] Documentation fixtures assert field names and stable error codes.

## Verification

Run unit and process-level CLI/server tests. Snapshot representative JSON only where stable and avoid timestamps/paths that make fixtures brittle. Explicitly hash temporary databases/cache trees before and after every read-only diagnostic test.

## Handoff notes

Task 20 will update `docs/agent-cli.md` only after Task 19 confirms the final command names and fields.
