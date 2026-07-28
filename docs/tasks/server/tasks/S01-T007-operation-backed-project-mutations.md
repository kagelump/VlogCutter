# S01-T007: Operation-Backed Project Mutations

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T007",
  "epic_id": "S01",
  "title": "Operation-Backed Project Mutations",
  "slug": "operation-backed-project-mutations",
  "status": "complete",
  "task_class": "foundation",
  "depends_on": [
    "S01-T001"
  ],
  "principal_invariant": "Every project mutation (annotation, asset addition, removal, rotation, clock event, observation) is recorded as an immutable typed operation that can be replayed to reconstruct state — and existing catalogs upgrade without data loss.",
  "central_falsification": "Take a pre-operation catalog fixture, upgrade it, apply a sequence of typed operations (note write, label, removal, clock event), replay them against a fresh catalog, and assert byte-identical state. Then attempt to apply a conflicting concurrent operation and assert the conflict is surfaced rather than silently resolved.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Operation log → project catalog",
    "Operation log → sync protocol (S01-T008)"
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
      "docs/tasks/server/tasks/S01-T007-operation-backed-project-mutations.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/**"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/**"
    ],
    "integrator": [
      "apps/desktop/**",
      "apps/cli/**"
    ]
  },
  "forbidden_paths": [
    "apps/server/**",
    "core/rcdvlog-server/**",
    "core/rcdvlog-sync/**",
    "apps/web/**",
    "deploy/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-core",
    "pnpm --dir apps/desktop build",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 70 \\| \\[S01-T007\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Replace implicit state-replacement mutations with an immutable typed operation log
that can be replayed, synchronized, and audited — without changing current
desktop behavior or losing existing catalog data.

## Principal invariant

This task is correct when every project mutation is recorded as an immutable
typed operation that can be replayed to reconstruct state — and existing catalogs
upgrade without data loss.

## Inputs and outputs

Inputs:
- S01-T001 schema and migration foundation
- Existing annotation, asset, clock, and observation write paths

Outputs:
- Typed operation model: note, label, removal, rotation, clock event, asset addition, observation, representation manifest
- Operation log with monotonic sequence numbers per origin
- Compatibility adapters so existing write calls emit operations
- Replay-based state reconstruction
- Conflict detection (not resolution — surfacing is the deliverable)
- Catalog upgrade path for existing projects

## Frozen decisions

1. Operations are immutable and append-only.
2. Each origin maintains its own monotonic sequence.
3. Replay is deterministic — same operations in order produce identical state.
4. Unsafe chronology concurrency is surfaced, not silently resolved.
5. Existing catalogs upgrade through the operation migration path without data loss.
6. Desktop saves remain local and never wait for network round trips.

## Owned paths

- **Implementer:** `core/rcdvlog-core/src/` operation types, log, replay, compatibility adapters
- **Independent tester:** `core/rcdvlog-core/tests/` with pre-operation catalog fixtures
- **Integrator:** `apps/desktop/` (adopt operation-emitting write paths), `apps/cli/` (inspection)

## Required interface

- Operation types and sequence model consumed by S01-T008 (sync protocol)
- Replay engine for catalog reconstruction
- Compatibility adapters so existing callers emit operations without rewrite

## Acceptance oracle

1. Existing catalogs upgrade without data loss.
2. Notes, labels, removals, rotations, and clock events emit typed operations.
3. Replaying operations against a fresh catalog produces byte-identical state.
4. Conflicting concurrent operations are surfaced, not silently resolved.
5. Desktop annotation writes succeed while server is unavailable.
6. Existing project and asset tests pass unchanged.

## Independent test charter

Try to falsify: "operations are a faithful record of state." Apply a sequence
of interleaved operations from multiple origins, replay them against a fresh
catalog, and assert byte equality with the original. Introduce a concurrent
conflict (two operations editing the same field with different values at the
same sequence) and assert the conflict is surfaced. Test upgrade of a
pre-operation catalog with real annotation data.

## Review charter

Risk domain: data integrity and upgrade safety. Questions: (1) Can upgrade lose
existing annotation data? (2) Is replay deterministic? (3) Are conflicts silently
resolved? (4) Do desktop writes still complete locally without network? Severity:
P0 for data loss during upgrade or non-deterministic replay; P1 for silent
conflict resolution. Approval vocabulary: `APPROVE`,
`APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Network sync protocol (S01-T008)
- Conflict resolution (S01-T011)
- Web chronology editor
- Operation log pruning or compaction

## Stop conditions

Stop and escalate when:

- An existing annotation write path cannot emit an operation without changing its API
- Upgrade of a real-world catalog fixture loses data
- Replay is non-deterministic due to wall-clock or external state

## Commands

```sh
cargo test -p rcdvlog-core
pnpm --dir apps/desktop build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Operation types and sequence model are the foundation for S01-T008 (sync protocol),
S01-T011 (conflict handling), S01-T015 (web annotations), and T01-T005 (server-origin projects).
