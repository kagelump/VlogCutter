# S01-T011: Automatic Incremental Sync and Conflict Handling

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T011",
  "epic_id": "S01",
  "title": "Automatic Incremental Sync and Conflict Handling",
  "slug": "automatic-sync-and-conflict-handling",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "S01-T008",
    "S01-T010"
  ],
  "principal_invariant": "After baseline adoption, the desktop and server reconcile automatically on reconnect — offline edits on either side converge, duplicate delivery is idempotent, and unsafe chronology concurrency is surfaced rather than silently resolved.",
  "central_falsification": "After baseline adoption, edit notes on desktop while offline, edit different notes on web, reconnect, and assert both sets of edits converge on both replicas. Then edit the same field on both sides while disconnected, reconnect, and assert the conflict is surfaced with both values preserved rather than silently choosing one.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Auto-sync → sync protocol (S01-T008)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md"
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
      "docs/tasks/server/tasks/S01-T011-automatic-sync-and-conflict-handling.md"
    ],
    "implementer": [
      "core/rcdvlog-sync/src/**",
      "apps/desktop/src-tauri/**"
    ],
    "independent_tester": [
      "core/rcdvlog-sync/tests/**"
    ],
    "integrator": [
      "apps/server/src/lib.rs",
      "apps/web/src/**"
    ]
  },
  "forbidden_paths": [
    "core/rcdvlog-core/src/catalog/**",
    "deploy/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-sync",
    "cargo test -p rcdvlog-server",
    "pnpm --dir apps/desktop build",
    "pnpm --dir apps/web build",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 110 \\| \\[S01-T011\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Make synchronization automatic after baseline adoption, with continuous
reconciliation on reconnect, and implement conflict detection and surfacing for
concurrent edits — without silent resolution.

## Principal invariant

This task is correct when after baseline adoption, the desktop and server
reconcile automatically on reconnect — offline edits on either side converge,
duplicate delivery is idempotent, and unsafe chronology concurrency is surfaced
rather than silently resolved.

## Inputs and outputs

Inputs:
- S01-T008 sync protocol and server replicas
- S01-T010 desktop pairing and baseline adoption
- S01-T007 operation log and conflict primitives

Outputs:
- Automatic sync trigger on desktop reconnect
- Continuous reconciliation during active connection
- Web-initiated edits synced to desktop
- Conflict detection for concurrent edits to same fields
- Conflict surfacing with both values preserved
- Sync status indicators on desktop and web
- HTTP recovery from missed WebSocket events
- Idempotent sync across timeout/retry

## Frozen decisions

1. Desktop saves remain local and never block on network.
2. Sync is triggered by connectivity events, not by a timer.
3. Conflicts are surfaced with both values — no automatic resolution.
4. Unsafe chronology concurrency is surfaced, not silently resolved.
5. WebSocket events are an optimization; HTTP catch-up is the fallback.
6. Duplicate delivery is idempotent — same operation, same sequence, no duplicate state.

## Owned paths

- **Implementer:** `core/rcdvlog-sync/` (auto-sync engine, conflict detection), `apps/desktop/src-tauri/` (sync client integration)
- **Independent tester:** `core/rcdvlog-sync/tests/` with concurrent-offline-edit scenarios
- **Integrator:** `apps/server/src/lib.rs` (WebSocket events, sync routes), `apps/web/src/` (web-initiated sync)

## Required interface

- Sync status events consumed by desktop and web UIs
- Conflict DTOs for user-facing surfacing
- Automatic sync configuration and diagnostics

## Acceptance oracle

1. Desktop offline edits sync automatically on reconnect.
2. Web edits appear on desktop after sync.
3. Desktop and web edits to different fields both converge.
4. Concurrent edits to the same field are surfaced as conflict with both values.
5. Duplicate delivery does not duplicate state.
6. Interrupted sync resumes and converges.
7. Sync is idempotent across timeout and retry.
8. HTTP catch-up works when WebSocket events are missed.
9. Sync status is visible and unambiguous on both surfaces.

## Independent test charter

Try to falsify: "conflicts are never silently resolved." Create concurrent edits
to the same field from desktop (offline) and web, reconnect, and assert the
conflict is surfaced with both values intact — never silently choosing one.
Test rapid interleaving of operations from three origins. Test sync interruption
at every stage of the protocol and verify convergence on resume. Test that
duplicate delivery of every operation type produces no duplicate state.

## Review charter

Risk domain: sync convergence and conflict integrity. Questions: (1) Can
concurrent edits be silently resolved? (2) Can duplicate delivery cause duplicate
state? (3) Can interrupted sync leave inconsistent state? (4) Is sync status
visible? Severity: P0 for silent conflict resolution or duplicate state; P1 for
inconsistent state after interrupted sync or invisible sync status. Approval
vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Conflict resolution UI (surfacing only)
- Chronology editor or merge tools
- Real-time collaborative editing
- Multi-master conflict resolution policies

## Stop conditions

Stop and escalate when:

- A conflict is automatically resolved without user visibility
- Sync polling is needed because connectivity events are unreliable
- WebSocket events become required rather than an optimization
- Conflict surfacing needs to change the operation model from S01-T007

## Commands

```sh
cargo test -p rcdvlog-sync
cargo test -p rcdvlog-server
pnpm --dir apps/desktop build
pnpm --dir apps/web build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Auto-sync and conflict surfacing are consumed by S01-T012 (remote representation
exchange), S01-T015 (web annotations), S01-T016 (sync administration), and
T01-T006 (mobile sync).
