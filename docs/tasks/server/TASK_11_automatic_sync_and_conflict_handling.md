# Task 11: Automatic Sync and Conflict Handling

**Status:** Planned
**Depends on:** Tasks 8 and 10
**Unblocks:** Tasks 12, 15, 16, and 18

## Objective

Run continuous, offline-tolerant desktop/server reconciliation; surface sync health and domain conflicts; and keep normal desktop mutations independent from network availability.

## User outcome

The user can annotate offline, reconnect through either LAN or Tailscale, and see desktop and web changes converge automatically. Ordinary sync stays quiet; stalled, incompatible, or conflicting state becomes inspectable and actionable.

## Scope

- Add desktop background reconcile loop with immediate wakeup after local operations.
- Add endpoint selection/failover between configured LAN and Tailscale aliases for the same server.
- Add exponential backoff, jitter, bounded batches, retry, and periodic full reconciliation.
- Add authenticated WebSocket hints plus HTTP recovery/polling.
- Synchronize human operations, node observations, and representation manifests.
- Materialize server-origin web operations on the desktop.
- Add sync status, pending counts, last success/error, endpoint, compatibility, and conflict projections.
- Add unobtrusive global/project UI status and detailed diagnostics panel.
- Add note revision diagnostics and explicit clock-conflict reporting.
- Add “Sync now” without making it required for correctness.

## Out of scope

- Web chronology conflict resolver
- Automatic server migration
- Sync pause per project; automatic sync is the accepted default for paired projects
- Media blob transfer
- Multiple Mac origins
- Operation pruning/compaction

## Design requirements

1. A desktop domain mutation commits locally before it signals the sync loop.
2. Network failure never rolls back a committed local mutation.
3. Exactly one reconcile loop runs per paired project within the desktop process.
4. All delivery is at-least-once and application is idempotent.
5. WebSocket messages are hints; reconnect always uses HTTP cursor reconciliation.
6. Endpoint failover verifies the same server ID before sending credentials or operations.
7. Backoff resets after success and remains bounded so long outages do not spin.
8. Sync status is derived from durable cursors plus current connection state, not optimistic UI guesses.
9. Applying completed asset operations replaces the React asset cache with canonical projections without losing current selection/focus.
10. Node availability remains node-relative and a disconnected node does not erase the other node's observations.
11. Note revisions are retained; MVP UI may show the active note and expose history through diagnostics.
12. Unresolved clock conflicts stop unsafe chronology materialization and identify both competing changes.
13. Protocol incompatibility pauses network sync but leaves local project interaction enabled.
14. The loop releases database connections promptly and does not hold a transaction during network I/O.
15. Desktop shutdown cancels sync cleanly without losing operations.

## Likely implementation areas

- `core/rcdvlog-sync` client reconciliation and status model
- `apps/desktop/src-tauri` async sync manager and events
- `apps/desktop/src/api.ts`, `types.ts`, `App.tsx`, and `styles.css`
- `apps/server` WebSocket/event and authenticated reconcile integration
- `core/rcdvlog-core` conflict/diagnostic projections

## Acceptance criteria

- [ ] Desktop notes/labels/removal/rotation commit while the server is offline.
- [ ] Offline Mac operations and server-origin operations converge after reconnect.
- [ ] Duplicate requests, lost responses, process restart, and WebSocket loss do not duplicate or lose state.
- [ ] LAN-to-Tailscale and Tailscale-to-LAN failover uses the same server pairing and resumes cursors.
- [ ] A malicious/different server at a configured URL is rejected by server ID.
- [ ] Sync status accurately reports unpaired, offline, connecting, syncing, synced, attention, incompatible, and credential-revoked states.
- [ ] Pending operation counts survive desktop restart.
- [ ] Note revisions both survive a concurrent edit and the deterministic active note matches on both replicas.
- [ ] Label add/remove, removed, and rotation convergence matches Task 7 rules.
- [ ] Clock conflicts are visible and no silent winner changes chronology.
- [ ] Project review/focus/playback remains responsive during a large reconciliation.
- [ ] Local database transactions are not held across HTTP requests.
- [ ] Explicit “Sync now” accelerates reconciliation but is not required for eventual convergence.

## Verification

Use a deterministic network fault harness to drop, delay, duplicate, and reorder responses. Run two-database convergence tests, desktop type/build checks, and a real desktop/server smoke test over both LAN and Tailscale routes.

## Handoff notes

Expose stable sync-status and conflict APIs for Tasks 16 and 18. Task 12 owns media transfer and must not be folded into the operation reconciliation transaction.
