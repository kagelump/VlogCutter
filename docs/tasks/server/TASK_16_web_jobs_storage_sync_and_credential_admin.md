# Task 16: Web Jobs, Storage, Sync, and Credential Administration

**Status:** Planned
**Depends on:** Tasks 4, 5, 9, 11, and 13
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
11. Administrative mutations produce structured audit events without secrets.
12. Dangerous-looking but unsupported destructive controls are absent, not merely disabled.
13. All forms are keyboard accessible and preserve errors/focus predictably.

## Likely implementation areas

- `core/rcdvlog-server` administrative commands and projections
- `apps/server` authenticated admin routes and audit logging
- `packages/project-client`
- `apps/web` settings/jobs/sync/credentials views
- WebSocket subscription/query cache integration
- Component, API, and Playwright tests

## Acceptance criteria

- [ ] A configured allowed root can be registered, verified, and disabled without filesystem mutation.
- [ ] An arbitrary/disallowed path and a traversal/symlink escape are rejected.
- [ ] Manual and scheduled scans are visible with durable progress and per-file failures.
- [ ] Proxy/thumbnail batch requests show reused, queued, succeeded, and failed counts.
- [ ] Job retry/cancel rules match Task 4 and survive server restart.
- [ ] WebSocket loss does not leave permanently stale job or sync status.
- [ ] Project sync overview matches durable cursor/pending/conflict state.
- [ ] Sessions and desktop credentials can be listed and individually revoked.
- [ ] The current browser session revokes cleanly; a desktop revocation leaves desktop local work intact.
- [ ] No UI/API can delete projects, originals, representations, or storage history.
- [ ] Server prerequisites, versions, and disk/derived-root status are visible.
- [ ] Chrome layout, focus, validation, confirmation, empty, loading, and failure states pass visual/interaction checks.

## Verification

Run server API tests, component tests, and Chrome Playwright administration flows. Exercise restart recovery for scans/jobs, disconnected mounts, revoked credentials, WebSocket loss, and a protocol mismatch fixture.

## Handoff notes

Task 17 supplies real backup/restore operations and deeper health. Task 18 mirrors agent-relevant projections through stable CLI JSON rather than requiring UI scraping.
