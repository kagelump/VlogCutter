# S01-T010: Desktop Pairing, Root Mapping, and Baseline Adoption

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T010",
  "epic_id": "S01",
  "title": "Desktop Pairing, Root Mapping, and Baseline Adoption",
  "slug": "desktop-pairing-root-mapping-and-baseline",
  "status": "complete",
  "task_class": "integration",
  "depends_on": [
    "S01-T008",
    "S01-T009"
  ],
  "principal_invariant": "The desktop pairs with the server through an explicit user-confirmed flow, maps its local storage roots to the server's corresponding logical roots, and the initial baseline adoption imports the exact project state without data loss or duplication.",
  "central_falsification": "Pair a desktop with a server, map a storage root with different Mac and Linux mount paths, initiate baseline adoption of an existing project, and assert the server replica matches the desktop catalog exactly (same assets, annotations, clock events). Then verify the desktop can work offline and reconcile on reconnect.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Desktop → pairing endpoints (S01-T009)",
    "Desktop → sync protocol (S01-T008)",
    "Storage root mapping → global media library (S01-T003)"
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
      "docs/tasks/server/tasks/S01-T010-desktop-pairing-root-mapping-and-baseline.md"
    ],
    "implementer": [
      "apps/desktop/src-tauri/**",
      "apps/desktop/src/**"
    ],
    "independent_tester": [
      "apps/desktop/tests/**"
    ],
    "integrator": [
      "core/rcdvlog-sync/**",
      "core/rcdvlog-server/src/**"
    ]
  },
  "forbidden_paths": [
    "apps/web/**",
    "apps/server/src/lib.rs",
    "deploy/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-sync",
    "pnpm --dir apps/desktop build",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 100 \\| \\[S01-T010\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement the desktop-side pairing flow, storage-root association between Mac and
server mount paths, and initial baseline project adoption — the first moment a
desktop project becomes visible on the server.

## Principal invariant

This task is correct when the desktop pairs with the server through an explicit
user-confirmed flow, maps its local storage roots to the server's corresponding
logical roots, and the initial baseline adoption imports the exact project state
without data loss or duplication.

## Inputs and outputs

Inputs:
- S01-T008 sync protocol and server project replicas
- S01-T009 authentication and pairing endpoints
- S01-T003 storage roots and logical bindings

Outputs:
- Desktop pairing UI: discover server, confirm pairing, store device credential
- Storage-root association: map Mac mount paths to server logical roots
- Baseline adoption: explicit preview, then import full project state to server
- Root-association evidence and confirmation contracts
- Server replica initialization from baseline

## Frozen decisions

1. Pairing requires explicit user confirmation on both sides.
2. The desktop never waits for a network round trip to commit an ordinary local edit — pairing and baseline are setup-time operations, not per-edit.
3. Storage roots with different Mac and Linux mount paths map through logical root IDs.
4. Baseline adoption is idempotent; re-running it for an already-adopted project is a no-op.
5. The server never connects inbound to the Mac.
6. A root the Mac cannot resolve is surfaced as unresolved, not silently dropped.

## Owned paths

- **Implementer:** `apps/desktop/src-tauri/` (pairing, root mapping, baseline commands), `apps/desktop/src/` (pairing UI, root association UI, baseline preview)
- **Independent tester:** `apps/desktop/` pairing and baseline tests
- **Integrator:** `core/rcdvlog-sync/` (baseline import protocol), `core/rcdvlog-server/src/` (server-side baseline reception)

## Required interface

- Pairing and root-association API consumed by desktop
- Baseline adoption protocol (builds on S01-T008 sync protocol)
- Root-association evidence DTOs consumed by S01-T014 (web root display)

## Acceptance oracle

1. Desktop discovers server and completes pairing with explicit user confirmation.
2. Pairing credential survives desktop restart.
3. User maps a Mac mount path to an existing server logical root.
4. Different mount paths for the same root are accepted and correctly associated.
5. Baseline adoption preview shows project assets and root resolvability.
6. Adoption imports exact project state to server without data loss or duplication.
7. After adoption, desktop works offline and reconciles on reconnect.
8. Revoking the desktop credential on the server prevents further sync.
9. Unresolvable roots are surfaced, not silently dropped.

## Independent test charter

Try to falsify: "baseline adoption is faithful." Create a desktop project with
known assets, annotations, and clock events. Pair and adopt baseline. Compare
the server replica's replayed state against the desktop catalog — assert byte
equality for every field. Test root mapping with different Mac and Linux paths.
Test that a revoked credential prevents sync. Test that adoption of an
already-adopted project is a no-op.

## Review charter

Risk domain: adoption fidelity and pairing safety. Questions: (1) Can baseline
adoption lose or duplicate data? (2) Can pairing succeed without user
confirmation? (3) Do root mappings survive restart? (4) Does revocation prevent
further sync? Severity: P0 for data loss/duplication during adoption or
pairing without confirmation; P1 for lost root mappings or delayed revocation.
Approval vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Server-origin project creation (T01-T005)
- Multiple desktop pairing
- Automatic root discovery
- Continuous sync (S01-T011)

## Stop conditions

Stop and escalate when:

- Baseline adoption requires transferring a SQLite file instead of replaying operations
- Pairing needs the server to connect inbound to the Mac
- Root mapping cannot handle different mount paths for the same logical root

## Commands

```sh
cargo test -p rcdvlog-sync
pnpm --dir apps/desktop build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Baseline adoption machinery is the foundation for T01-T005's reverse
server-origin adoption. Root mapping contracts are consumed by S01-T014 (web).
