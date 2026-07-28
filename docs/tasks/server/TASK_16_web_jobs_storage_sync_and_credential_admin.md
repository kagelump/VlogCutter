# Task 16: Web Jobs, Storage, Sync, and Credential Administration

**Status:** Implemented (2026-07-28)
**Depends on:** Tasks 3, 4, 5, 6, 9, 11, and 13
**Unblocks:** Task 19

## Objective

Provide safe Chrome administration for configured storage, scans/schedules, durable jobs, synchronization health, sessions, and device credentials.

## User outcome

The user can operate the home node without editing config files or SQLite: mount bindings can be registered and diagnosed, scans/proxies can be launched and monitored, sync problems are understandable, and old sessions/devices can be revoked.

## Scope

- Storage-root list/create/edit-display-name/disable and server binding configuration.
- Binding verification, mount/access/containment status, and partial-root configuration.
- Manual scan launch and persisted schedule configuration.
- Job queue list/detail/progress/ETA/failure/retry/cancel controls.
- Batch thumbnail/proxy requests with execution location clearly shown.
- Sync overview by project: cursors, pending operations, last success/error, endpoint, compatibility, and conflicts.
- Browser session and paired desktop credential listing/revocation.
- Server version, prerequisite, capacity, and derived-root status.
- WebSocket live updates with polling recovery.
- Confirmations and audit records for administrative mutations.

## Out of scope

- Filesystem mount execution or editing `/etc/fstab` from the browser
- Deleting storage roots, media objects, projects, jobs, representations, originals, or backups
- Restore execution; Task 17 exposes an explicit server-side command
- Automatic cleanup
- User roles
- Web chronology conflict resolution
- Backup administration; Task 17 owns backups, and this task surfaces only whatever read-only
  backup health the readiness projection already exposes

## Root binding lifecycle

Task 3 separates two contracts, and the UI must not collapse them:

- `NodeRootCapability` is path-free and safe to synchronize. It carries the root and node IDs,
  optional `rootSubpath`, protocol, access mode, enabled/verification state, association
  evidence, and confirmation state.
- `NodeRootBindingAdmin` adds this server's absolute `localMountPath` and is reachable only
  through local administrative methods.

A binding is usable for filesystem resolution only when the root is enabled and the binding is
enabled, verified, and explicitly `confirmed`. The administration flow therefore has to show
the association evidence — relative-path, quick-fingerprint, and full-hash match counts,
mismatch count, and sampled relative paths — and require an explicit confirm or reject. It
must not auto-confirm on the strength of a path that merely looks right, and an immutable
original root accepts only a `read_only` binding.

## Schedule semantics

Persisted schedules run on a laptop that sleeps and reboots, so the contract must be explicit:

- Schedules store an IANA time zone, not a fixed UTC offset.
- A run missed while the server was down is coalesced into at most one catch-up run at
  startup; it never replays every skipped occurrence.
- A schedule never starts a run while its previous run is still active for the same scope.
- Schedule changes are administrative mutations with audit records like any other.

## Design requirements

1. The web UI configures only paths already allowed by deployment configuration; it cannot browse arbitrary server filesystem paths.
2. Root creation/association displays logical identity separately from the server absolute binding.
3. Disabling a root preserves all historical records and requires confirmation.
4. Manual scans and media batches use idempotency keys and show skipped/reused work.
5. Retry creates a new attempt only for an eligible failed job; cancel is durable.
6. Progress distinguishes discovery, queued work, completed work, failures, worker count, and ETA.
7. A stale WebSocket view refetches durable job/sync state.
8. Credential revocation shows the exact session/device metadata and cannot reveal the token.
9. Revoking the current browser session logs it out after the server commits revocation.
10. Sync diagnostics separate stored state, live connectivity, node observations, conflicts, and recommendations.
11. Administrative mutations produce structured audit events without secrets. Audit records are durable rows in the control catalog with their own migration, actor, request ID, and bounded detail — not log lines that rotation can erase.
12. Dangerous-looking but unsupported destructive controls are absent, not merely disabled.
13. All forms are keyboard accessible and preserve errors/focus predictably.
14. Representation batches request only full-hash-verified media; unverified selections report `source_not_verified` as a skipped reason rather than enqueuing work that cannot run.
15. Every new endpoint updates the committed OpenAPI document and generated TypeScript client.

## Likely implementation areas

- `core/rcdvlog-server` administrative commands and projections
- `apps/server` authenticated admin routes and audit logging
- `packages/project-client`
- `apps/web` settings/jobs/sync/credentials views
- WebSocket subscription/query cache integration
- Component, API, and Playwright tests

## Acceptance criteria

- [x] A configured allowed root can be registered, verified, and disabled without filesystem mutation.
- [x] An arbitrary/disallowed path and a traversal/symlink escape are rejected.
- [x] Manual and scheduled scans are visible with durable progress and per-file failures.
- [x] Proxy/thumbnail batch requests show reused, queued, succeeded, and failed counts.
- [x] Job retry/cancel rules match Task 4 and survive server restart.
- [x] WebSocket loss does not leave permanently stale job or sync status.
- [x] Project sync overview matches durable cursor/pending/conflict state.
- [x] Sessions and desktop credentials can be listed and individually revoked.
- [x] The current browser session revokes cleanly; a desktop revocation leaves desktop local work intact.
- [x] No UI/API can delete projects, originals, representations, or storage history.
- [x] Server prerequisites, versions, and disk/derived-root status are visible.
- [x] Chrome layout, focus, validation, confirmation, empty, loading, and failure states pass visual/interaction checks.
- [x] A binding stays unusable for resolution until its evidence is shown and it is explicitly confirmed; rejecting it leaves the root and its history intact.
- [x] An immutable original root refuses a read-write binding through the UI and the API.
- [x] A schedule missed across a server outage produces one catch-up run, and a schedule never overlaps its own previous run.
- [x] Administrative mutations leave durable audit rows containing the request ID and no secrets, and those rows survive restart.
- [x] A batch containing unverified media reports skipped counts with reasons instead of enqueuing unrunnable jobs.

## Verification

Run server API tests, component tests, and Chrome Playwright administration flows. Exercise restart recovery for scans/jobs, disconnected mounts, revoked credentials, WebSocket loss, and a protocol mismatch fixture.

## Handoff notes

Task 17 supplies real backup/restore operations and deeper health. Task 18 mirrors agent-relevant projections through stable CLI JSON rather than requiring UI scraping.
