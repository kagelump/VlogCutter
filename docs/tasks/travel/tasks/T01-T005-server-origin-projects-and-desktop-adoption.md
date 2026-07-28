# T01-T005: Server-Origin Project Creation and Desktop Adoption

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "T01-T005",
  "epic_id": "T01",
  "title": "Server-Origin Project Creation and Desktop Adoption",
  "slug": "server-origin-projects-and-desktop-adoption",
  "status": "ready",
  "task_class": "foundation",
  "depends_on": [],
  "principal_invariant": "A project created on the server by an authenticated device is indistinguishable in behavior from a desktop-created project after desktop adoption, and no code path branches on project origin to decide behavior.",
  "central_falsification": "Create a project on the server via device credential, adopt it on the desktop, and run the full desktop test suite against it — every test that passes for a desktop-created project must also pass for the adopted project. Then assert that no code path branches on project origin.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Project creation → operation log → sync protocol (S01-T008)",
    "Desktop adoption → baseline import machinery (S01-T010)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "independent_tester": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/travel/ARCHITECTURE.md",
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/travel/tasks/T01-T005-server-origin-projects-and-desktop-adoption.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/**",
      "core/rcdvlog-sync/src/**",
      "core/rcdvlog-server/src/replicas.rs",
      "apps/server/src/lib.rs",
      "apps/desktop/src-tauri/**"
    ],
    "independent_tester": [
      "core/rcdvlog-sync/tests/**",
      "apps/desktop/tests/**"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "forbidden_paths": [
    "apps/web/**",
    "apps/mobile/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-core",
    "cargo test -p rcdvlog-sync",
    "cargo test -p rcdvlog-server",
    "pnpm --dir apps/desktop build",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/travel/EPIC.md",
      "regex": "\\| 50 \\| \\[T01-T005\\].*\\| complete \\|"
    },
    {
      "path": "docs/tasks/server/ARCHITECTURE.md",
      "regex": "Projects may be created on the desktop or on the server"
    }
  ]
}
-->

## Goal

Allow a project to be created on the server by an authenticated device, and allow
the desktop to adopt a server-created project as a full local replica — the
mirror image of the existing desktop-to-server baseline import.

## Principal invariant

This task is correct when a project created on the server by an authenticated
device is indistinguishable in behavior from a desktop-created project after
desktop adoption, and no code path branches on project origin to decide behavior.

## Inputs and outputs

Inputs:

- S01-T007 operation-backed project mutations
- S01-T008 sync protocol and server project replicas
- S01-T010 desktop pairing, root mapping, and baseline adoption

Outputs:

- `project.create` operation with server origin, callable by device credential
- Server-side project replica creation without prior desktop baseline
- Origin node recorded on project for diagnostics
- Server API listing projects with no local replica
- Desktop "Adopt server project": preview, then materialize local catalog from operation log
- Architecture document amendment: project creation is no longer desktop-only

## Frozen decisions

1. `project.create` is an ordinary immutable operation, not a side channel.
2. The server allocates a `projectId` that is stable and globally unique.
3. A device credential is sufficient authority; no new credential class.
4. Creation requires connectivity. Annotation does not.
5. Adoption replays operations; it never transfers a SQLite file.
6. A half-materialized catalog is never presented as usable.
7. Adoption is idempotent; re-running it for an adopted project is a no-op.
8. After adoption, no code path may branch on project origin to decide behavior.
9. Existing desktop-to-server baseline import continues to work unchanged.
10. Storage roots the Mac cannot resolve appear as unresolved, and their assets remain useful.

## Owned paths

- **Implementer:** `core/rcdvlog-core/` (project.create operation, origin field), `core/rcdvlog-sync/` (project listing, adoption DTOs), `core/rcdvlog-server/src/replicas.rs` (server-origin replica creation), `apps/server/src/lib.rs` (project creation route, unadopted-project listing), `apps/desktop/src-tauri/` (adoption command, local catalog materialization)
- **Independent tester:** `core/rcdvlog-sync/tests/`, `apps/desktop/` adoption tests
- **Integrator:** `docs/tasks/server/ARCHITECTURE.md` (invariant amendment)

## Required interface

- `project.create` operation type in shared DTOs
- Adoption listing and preview API
- Desktop adoption command (Tauri command)

## Acceptance oracle

1. A device credential can create a project on the server.
2. The created project has a stable `projectId` and recorded origin node.
3. The server creates its replica without requiring a desktop baseline.
4. The desktop lists server projects it has not adopted.
5. The adoption preview reports assets, operations, roots, and root resolvability.
6. Adoption materializes a local catalog by replaying operations, not by file transfer.
7. An interrupted adoption never leaves a usable half-materialized project.
8. Re-running adoption for an adopted project is a no-op.
9. An adopted project supports every desktop operation a locally created project supports.
10. No behavior branches on project origin after adoption.
11. Existing desktop-to-server baseline import is unaffected.
12. Unresolvable storage roots are surfaced, and their assets remain useful.
13. Project deletion remains unavailable on every surface.
14. The server architecture document no longer contradicts this behavior.

## Independent test charter

Try to falsify: "an adopted project is indistinguishable from a desktop-created
one." Create a project on the server from synthetic operations, adopt it on the
desktop, and assert the resulting catalog is equivalent to one built by applying
the same operations locally. Kill the desktop mid-adoption and assert recovery
leaves no half-materialized catalog. Include a project referencing a storage root
the test Mac cannot mount. Run the full desktop test suite against the adopted
project.

## Review charter

Risk domain: project identity and adoption safety. Questions: (1) Can adoption
produce a catalog that behaves differently from a desktop-created one? (2) Can a
half-materialized catalog be opened as usable? (3) Does any code path branch on
project origin? (4) Is the architecture document updated? Severity: P0 for
behavioral divergence or origin-branching code; P1 for half-materialized catalog
visibility or missing architecture update. Approval vocabulary: `APPROVE`,
`APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Project deletion on any surface
- Multiple desktops adopting the same project
- Project transfer or origin reassignment
- Trip context, chapters, or events
- Card inventory and provisional assets (T01-T007)

## Stop conditions

Stop and escalate when:

- The adoption flow needs to transfer a SQLite file instead of replaying operations
- Project origin must be checked to decide any behavior after adoption
- The architecture document amendment is contested
- Desktop baseline import must be changed rather than mirrored

## Commands

```sh
cargo test -p rcdvlog-core
cargo test -p rcdvlog-sync
cargo test -p rcdvlog-server
pnpm --dir apps/desktop build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

T01-T006 and T01-T007 both assume a trip project already exists and was created
from the phone. The creation API shape defined here is what the phone calls
first, before any card is touched.
