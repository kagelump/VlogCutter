# Task 8: Sync Protocol and Server Project Replicas

**Status:** Implemented 2026-07-23
**Depends on:** Tasks 2, 3, and 7
**Unblocks:** Tasks 10, 11, 14, 17, and 18

## Objective

Implement the transport-independent synchronization protocol, server project-replica lifecycle, atomic baseline import, and idempotent incremental reconciliation.

## User outcome

A desktop-created project can have one durable server replica that accepts the same domain operations and can converge after either side has accumulated changes, without copying the desktop SQLite file.

## Scope

- Add `core/rcdvlog-sync` with protocol DTOs, compatibility negotiation, cursor logic, batching, and reconciliation services.
- Add server project registry and one local-SSD replica database per `projectId`.
- Implement baseline export/import format, preview data, digest, idempotency, and atomic replica registration.
- Implement per-origin contiguous cursors for Mac and server operations.
- Implement bounded incremental push/pull reconciliation.
- Include node observations and representation manifests as separate synchronization classes.
- Permit server-origin project asset additions using the same project-domain command layer.
- Link desktop-ingested project assets to existing or provisional global media and return that association through a server-origin operation.
- Add internal HTTP baseline/reconcile routes; Task 9 authenticates them.
- Add compatibility errors for project/server/protocol/schema mismatch.

## Out of scope

- Desktop pairing UI and background loop
- Public credentials and TLS
- Web UI
- More than one Mac origin
- Server migration or multiple origins beyond Mac/server
- Operation compaction or deletion
- Media blob transfer

## Baseline requirements

The versioned baseline includes only one selected `projectId`, even if its desktop catalog contains other projects. It includes:

- Project identity and metadata
- Current project assets and project-scoped state
- Current clock events and unresolved conflicts
- Operation/materialization watermark
- Storage-root-relative projections where known
- Existing representation manifests and node-local availability facts
- Schema and protocol versions

The desktop produces a digest over canonical baseline bytes. The server imports into a temporary replica, validates referential integrity and counts, then atomically registers it. An existing replica with a different baseline is never overwritten.

## Incremental protocol requirements

1. Request includes authenticated identities later supplied by Task 9, protocol range, project schema, per-origin cursors, bounded new operations, observations, and manifests.
2. The server validates all operation sequences and payload versions before acknowledging them.
3. Append/materialize/acknowledge occurs transactionally within the target project replica.
4. Response includes committed Mac cursor, missing server-origin operations, updated fact cursors, conflicts, and pagination continuation.
5. Repeated requests and responses are idempotent.
6. Unknown gaps do not advance a contiguous cursor.
7. A server-origin web operation uses the stable server node ID and server sequence.
8. A project can pair with exactly one server ID. A mismatch requires an explicit future migration workflow and is rejected here.
9. Unknown major protocol or operation versions pause sync. Additive compatible minor versions proceed.
10. No API accepts or emits a raw SQLite database as the ongoing sync format.
11. Absolute mount paths are excluded from canonical project payloads.
12. Batch count/byte limits prevent one project from monopolizing the server.
13. A project operation is authoritative for project membership; the control-catalog membership index is an idempotent, repairable projection rather than a cross-database transaction.

## Likely implementation areas

- `core/rcdvlog-sync/`
- `core/rcdvlog-core` baseline projection/import and operation interfaces
- `core/rcdvlog-server` project registry and replica manager
- `apps/server` internal sync routes
- Temporary two-replica integration tests

## Acceptance criteria

- [x] A selected project from a multi-project legacy catalog exports only that project.
- [x] Baseline preview reports stable counts, proposed library matches, root mappings, and conflicts without mutating server state.
- [x] Confirmed baseline import produces an equivalent server materialized project.
- [x] Interrupted baseline import leaves no registered partial replica and is safe to retry.
- [x] A different baseline cannot overwrite an existing project replica.
- [x] Offline Mac and server operation sets converge after bounded reconciliation.
- [x] Delivery order, duplicate requests, response loss, and reconnect do not duplicate or lose operations.
- [x] Sequence gaps prevent cursor advancement and return an actionable protocol error.
- [x] Node observations supersede only their own origin/scope.
- [x] Representation manifests merge without treating job progress as project state.
- [x] A server-library asset added to a project materializes on both replicas with one stable `projectAssetId`.
- [x] A desktop-ingested asset synchronizes before global verification, then receives the same server-assigned media link on both replicas without changing `projectAssetId`.
- [x] A crash between project commit and control-catalog projection is repaired from the source operation without duplicating membership.
- [x] Protocol/schema incompatibility is explicit and leaves both replicas readable.
- [x] No live SQLite or WAL file is transferred or opened across nodes.

## Verification

Add deterministic integration tests with independent temporary Mac and server databases, simulated disconnects, shuffled operations, duplicated pages, and process restart. Property-test convergence for supported operation combinations.

## Handoff notes

Freeze the v1 baseline and reconcile envelopes before Tasks 10 and 11. Generate or commit their JSON schemas so desktop and server integration failures are caught at build/test time.
