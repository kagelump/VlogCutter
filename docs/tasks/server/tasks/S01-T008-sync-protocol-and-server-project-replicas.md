# S01-T008: Sync Protocol and Server Project Replicas

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T008",
  "epic_id": "S01",
  "title": "Sync Protocol and Server Project Replicas",
  "slug": "sync-protocol-and-server-project-replicas",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "S01-T002",
    "S01-T003",
    "S01-T007"
  ],
  "principal_invariant": "The server maintains an independent project replica synchronized through per-origin operation sequences — SQLite files and changesets are never the synchronization protocol, and duplicate delivery does not duplicate state.",
  "central_falsification": "Create a project on the desktop with known operations, synchronize to the server, verify the server replica's replayed state matches the desktop's, apply offline edits on the desktop, synchronize again, and verify convergence. Then deliberately deliver the same operation twice and assert no duplicate state.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Sync protocol → operation log (S01-T007)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/SYNC_PROTOCOL.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/SYNC_PROTOCOL.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T008-sync-protocol-and-server-project-replicas.md"
    ],
    "implementer": [
      "core/rcdvlog-sync/src/**",
      "core/rcdvlog-server/src/**"
    ],
    "independent_tester": [
      "core/rcdvlog-sync/tests/**"
    ],
    "integrator": [
      "apps/server/src/lib.rs",
      "apps/desktop/src-tauri/**"
    ]
  },
  "forbidden_paths": [
    "apps/web/**",
    "deploy/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-sync",
    "cargo test -p rcdvlog-server",
    "cargo test -p rcdvlog-core",
    "pnpm --dir apps/desktop build",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 80 \\| \\[S01-T008\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement the operation-based synchronization protocol between desktop and server,
server-side project replicas, and per-origin cursors — without using SQLite files
or changesets as the transport.

## Principal invariant

This task is correct when the server maintains an independent project replica
synchronized through per-origin operation sequences — SQLite files and changesets
are never the synchronization protocol, and duplicate delivery does not
duplicate state.

## Inputs and outputs

Inputs:
- S01-T007 typed operation log and sequence model
- S01-T002 server control catalog
- S01-T003 storage roots and global media library

Outputs:
- Operation-based sync protocol with per-origin cursors and bounded pages
- Server-side project replica creation and maintenance
- Server project registry in control catalog
- Sync routes: push operations, pull operations, cursor state
- Idempotent operation application
- Duplicate delivery detection and rejection
- Server-replica replay from operation log
- Sync status and diagnostics

## Frozen decisions

1. SQLite files and SQLite changesets are not the synchronization protocol.
2. Operations are the unit of synchronization.
3. Each origin maintains its own monotonic sequence cursor.
4. Sync is pull-based: the client requests operations it hasn't seen.
5. Duplicate delivery is detected and idempotently handled — no duplicate state.
6. The server maintains its own project replica, independently replayed.
7. Absolute paths appear only in local diagnostic projections, never in sync payloads.
8. Sync is idempotent across timeout/retry and converges after offline edits.

## Owned paths

- **Implementer:** `core/rcdvlog-sync/` (protocol, cursors, sync engine), `core/rcdvlog-server/src/` (server replica management)
- **Independent tester:** `core/rcdvlog-sync/tests/` with multi-origin operation sequences
- **Integrator:** `apps/server/src/lib.rs` (sync routes), `apps/desktop/src-tauri/` (sync client)

## Required interface

- Sync protocol DTOs and cursor model
- Server sync routes consumed by desktop and (later) phone
- Server replica services consumed by S01-T010 (baseline adoption)

## Acceptance oracle

1. Desktop operations synchronize to server and replay into a matching replica.
2. Offline desktop edits converge on reconnect.
3. Duplicate operation delivery does not duplicate state.
4. Sync is idempotent across timeout and retry.
5. Server replica replays operations, never receives a SQLite file.
6. Per-origin cursors track synchronization progress correctly.
7. No absolute path appears in any sync payload.
8. Sync continues normally while a large transfer runs (T01-T002's constraint).

## Independent test charter

Try to falsify: "synchronization is idempotent and convergent." Create a project
with known operations, sync to server, verify server replica matches. Apply
offline edits on desktop, sync, verify convergence. Deliberately deliver
duplicate operations and assert no duplicate state. Interrupt sync mid-stream and
verify resume converges. Test with operations from multiple origins interleaved.
Verify absolute paths never appear in sync payloads.

## Review charter

Risk domain: synchronization correctness. Questions: (1) Can duplicate delivery
cause duplicate state? (2) Is a SQLite file ever transferred as the sync
mechanism? (3) Can interrupted sync leave inconsistent state? (4) Do absolute
paths leak into sync payloads? Severity: P0 for duplicate state or SQLite-file
transfer; P1 for interrupted-sync inconsistency or path leakage. Approval
vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Conflict resolution (S01-T011)
- Multi-server peer-to-peer sync
- Real-time push notifications (WebSocket events are an optimization, not the protocol)
- Sync of raw media files (that's the transfer protocol, T01-T002)

## Stop conditions

Stop and escalate when:

- The sync protocol needs to transfer SQLite files or changesets
- An absolute path is required in a sync payload
- The protocol cannot distinguish duplicate delivery from a genuine repeated operation
- Per-origin cursor state cannot be recovered after restart

## Commands

```sh
cargo test -p rcdvlog-sync
cargo test -p rcdvlog-server
cargo test -p rcdvlog-core
pnpm --dir apps/desktop build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Sync protocol is consumed by S01-T010 (baseline adoption), S01-T011 (automatic sync),
S01-T012 (remote representation exchange), T01-T005 (server-origin projects), and
T01-T006 (mobile sync).
