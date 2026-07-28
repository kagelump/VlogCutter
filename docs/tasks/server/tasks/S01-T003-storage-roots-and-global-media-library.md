# S01-T003: Storage Roots and Global Media Library

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T003",
  "epic_id": "S01",
  "title": "Storage Roots and Global Media Library",
  "slug": "storage-roots-and-global-media-library",
  "status": "complete",
  "task_class": "foundation",
  "depends_on": [
    "S01-T001",
    "S01-T002"
  ],
  "principal_invariant": "The server can represent one logical storage root once even when Mac and Ubuntu mount it at different absolute paths, and media identity is never confused with project annotations.",
  "central_falsification": "Register a storage root, bind it with distinct Mac and Linux mount paths, create a media object through one binding, and assert the same media object is observable through the other binding with the same MediaObjectId and no cross-contamination of absolute paths in sync payloads.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Server control catalog → storage root model",
    "Storage root model → scan/sync/library consumers"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/STORAGE_LIBRARY.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/STORAGE_LIBRARY.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/STORAGE_LIBRARY.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/STORAGE_LIBRARY.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T003-storage-roots-and-global-media-library.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/**",
      "core/rcdvlog-server/src/**"
    ],
    "independent_tester": [
      "core/rcdvlog-server/tests/**"
    ],
    "integrator": [
      "Cargo.toml",
      "Cargo.lock",
      "apps/server/src/lib.rs"
    ]
  },
  "forbidden_paths": [
    "apps/desktop/**",
    "apps/web/**",
    "apps/cli/**",
    "core/rcdvlog-sync/**",
    "deploy/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server-core",
    "cargo test -p rcdvlog-server",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 30 \\| \\[S01-T003\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement the server-wide identity model for logical storage roots, node-local
bindings, global media objects, physical storage objects, project membership
links, and node-relative availability.

## Principal invariant

This task is correct when the server can represent one logical storage root once
even when Mac and Ubuntu mount it at different absolute paths, and media identity
is never confused with project annotations.

## Inputs and outputs

Inputs:

- Task 1 identity types and Task 2 server control catalog
- Existing project catalog schema

Outputs:

- Server control-catalog migrations for storage and global media identity
- Storage-root create/list/update-disable operations
- `NodeRootBinding` with local mount path, optional root subpath, protocol, access mode, verification status
- Project-independent `MediaObject` and root-relative `StorageObject` records
- Project membership linkage (annotations stay in project catalog)
- Safe root-relative path parsing and resolution
- Generation-based node availability observations
- Explicit root-association evidence and confirmation data contracts
- Paginated library query primitives and indexes

## Frozen decisions

1. `StorageRootId` is logical and stable; mount paths are binding-local.
2. A binding supports `rootSubpath` for subtree-only mounts.
3. Original roots marked immutable independently of filesystem permissions.
4. The application never writes `.rcdvlog-storage-root` or any marker into an original tree.
5. A root-relative path cannot escape its binding through traversal, alternate separators, symlinks, or encoding tricks.
6. Availability: known, accessible here, accessible on another node, temporarily unavailable, missing after completed observation, fingerprint mismatch.
7. An incomplete scan generation cannot mark prior observations missing.
8. Global media records allow nullable quick/full hashes and explicit verification states.
9. `ProjectAssetId` remains project-scoped; annotations never appear in global media tables.
10. One media object in multiple projects; multiple storage objects for one media object.
11. Absolute mount paths appear only in local administrative/diagnostic projections, never in sync payloads.
12. Disabling a root preserves historical identity and project links.

## Owned paths

- **Implementer:** `core/rcdvlog-core/` shared identity and availability DTOs; `core/rcdvlog-server/` control-catalog migrations, repositories, services
- **Independent tester:** `core/rcdvlog-server/tests/` plus path normalization and partial-root test fixtures
- **Integrator:** workspace `Cargo.toml`, `Cargo.lock`, `apps/server/src/lib.rs` internal admin routes (test-only until auth arrives in S01-T009)

## Required interface

- Storage root registration and binding services in `rcdvlog-server`
- Library query primitives (cursor-paginated, deterministic)
- Media object and storage object identity types in `rcdvlog-core`
- Availability observation and generation tracking

## Acceptance oracle

1. The same `storageRootId` resolves through distinct Mac and Linux bindings.
2. A partial binding resolves assets under `rootSubpath` and rejects assets outside it.
3. Traversal, absolute, symlink-escape, and malformed relative paths are rejected.
4. Two storage objects may point to one media object without becoming two project assets automatically.
5. Two project assets may reference one media object while retaining independent annotations.
6. A completed new observation generation supersedes only the originating node and scanned scope.
7. An interrupted generation leaves prior availability intact.
8. Disabling or unmounting a binding changes availability without deleting records.
9. Library queries are cursor-paginated and deterministic.
10. No test creates a marker or writes into an original root.
11. Control-catalog migration upgrades preserve server identity and project registry state.

## Independent test charter

Try to falsify: "storage root identity is independent of mount path." Create two
bindings for the same root with different absolute paths and different
`rootSubpath` values. Write a media object through one, verify it is reachable
through the other with identical identity. Try traversal attacks with `../`,
symlinks, alternate separators, and Unicode normalization variants. Verify no
absolute path leaks into a sync-payload-shaped DTO. Use temporary roots only;
verify an original root made filesystem-read-only rejects writes through the
domain service.

## Review charter

Risk domain: media identity integrity and path safety. Questions: (1) Can
different mount paths produce different media identities for the same file? (2)
Can a path traversal escape a binding's root? (3) Do annotations ever appear in
global media tables? (4) Can an incomplete observation generation invalidate
existing availability? Severity: P0 for identity confusion or path escape; P1
for annotation leakage or availability corruption. Approval vocabulary:
`APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Walking/scanning the filesystem
- Fingerprint or metadata jobs
- Root-association UI
- Project synchronization
- Representation storage
- Original relocation or marker-file creation

## Stop conditions

Stop and escalate when:

- A path normalization routine needs to follow symlinks across binding boundaries
- A storage root identity depends on absolute mount paths
- The design requires writing a marker file into an original tree
- Media object identity needs project-scoped data to be unique

## Commands

```sh
cargo test -p rcdvlog-server-core
cargo test -p rcdvlog-server
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Expose library/storage services through Rust interfaces consumed by Tasks 5, 8,
10, 14, and 16. Do not make transport DTOs the internal persistence model.
