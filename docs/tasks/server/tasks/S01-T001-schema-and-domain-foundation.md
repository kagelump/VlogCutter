# S01-T001: Schema and Domain Foundation

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T001",
  "epic_id": "S01",
  "title": "Schema and Domain Foundation",
  "slug": "schema-and-domain-foundation",
  "status": "complete",
  "task_class": "foundation",
  "depends_on": [],
  "principal_invariant": "Every existing project catalog opens identically as before, but the catalog now carries forward-migration history and stable identity fields that future server tasks can rely on.",
  "central_falsification": "Take a pre-migration catalog fixture matching the oldest supported schema, run Catalog::open, and assert that all existing project data survives intact while the schema_migrations table records exactly one baseline migration row — then reopen and confirm zero additional migrations applied.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Catalog::open → schema migration runner",
    "core identity types → all downstream crates"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/SCHEMA.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/SCHEMA.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/SCHEMA.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/SCHEMA.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T001-schema-and-domain-foundation.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/**",
      "core/rcdvlog-core/Cargo.toml"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/**"
    ],
    "integrator": [
      "Cargo.toml",
      "Cargo.lock",
      "apps/cli/src/main.rs",
      "apps/cli/tests/cli_workflow.rs",
      "apps/desktop/src/types.ts"
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
    "cargo test -p rcdvlog-cli",
    "pnpm --dir apps/desktop check",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 10 \\| \\[S01-T001\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Introduce the versioned migration and shared identity foundations required by the
companion server without changing current desktop behavior or losing
compatibility with existing project catalogs.

## Principal invariant

This task is correct when every existing project catalog opens identically as
before, but the catalog now carries forward-migration history and stable identity
fields that future server tasks can rely on.

## Inputs and outputs

Inputs:

- Existing `CREATE TABLE IF NOT EXISTS` catalog initialization
- Current project, asset, annotation, thumbnail, proxy, and clock schema
- Existing desktop, CLI, and test expectations

Outputs:

- Numbered, transactional forward migration system with a `schema_migrations` table
- Strongly typed identifiers: node, project, project asset, media object, storage root, storage object, representation, operation, job
- Project-level replica metadata (nullable server pairing fields, no pairing performed)
- Nullable project-asset linkage fields for future global media identity
- Serialization, validation, and error conventions shared across crates
- Legacy-schema upgrade fixtures

## Frozen decisions

1. Migrations are numbered, monotonic, transactional, and run once. Failure rolls back.
2. `Catalog::open` and writable `open_existing` migrate forward; read-only opens and diagnostics never migrate.
3. Unknown newer schemas are rejected with a stable error — no downgrade path.
4. Existing idempotent table creation becomes the baseline migration.
5. IDs remain opaque strings at JSON boundaries; Rust newtypes are validated wrappers only.
6. Nullable linkage rather than fabricating `mediaObjectId` before server verification.
7. Verification enum: `unlinked`, `provisional`, `verified`, `mismatch`.
8. DTOs use camelCase unless an existing public enum contract says otherwise.
9. Migrations may add indexes but must not rewrite media files or generated caches.

## Owned paths

- **Implementer:** `core/rcdvlog-core/src/` migration, identifier, and catalog modules; `core/rcdvlog-core/Cargo.toml`
- **Independent tester:** `core/rcdvlog-core/tests/`
- **Integrator:** workspace `Cargo.toml`, `Cargo.lock`, `apps/cli/src/main.rs` catalog inspection updates, `apps/cli/tests/cli_workflow.rs`, `apps/desktop/src/types.ts` new-field additions only

## Required interface

- Migration runner trait consumed by `Catalog::open`
- Identity types (`NodeId`, `ProjectId`, `ProjectAssetId`, `MediaObjectId`, etc.) exported from `rcdvlog-core`
- Schema version introspection for diagnostics and later server control-catalog conventions

## Acceptance oracle

1. A current catalog upgrades once and reports the expected schema version.
2. Reopening does not rerun migrations or change data.
3. A pre-orientation/pre-label fixture upgrades without data loss.
4. A deliberately failing migration rolls back completely.
5. A catalog with a newer unsupported schema fails with a stable actionable error.
6. Read-only opens and diagnostics do not create `schema_migrations` or modify the file.
7. Existing Project, Asset, AssetPatch, ingest, proxy, thumbnail, and chronology tests pass unchanged.
8. Existing project and asset ID strings are unchanged.
9. New nullable media-link fields serialize predictably and do not break old browser mocks.
10. No source or cache file is read or written as part of schema migration.

## Independent test charter

Try to falsify: "migrations are safe and idempotent." Use a real pre-migration
catalog fixture from the oldest supported schema. Run migration, reopen, verify
data. Run a failing migration mid-sequence, verify rollback leaves prior schema
intact. Run a catalog with a fabricated future schema version, verify rejection.
Run read-only open on an already-upgraded catalog, verify zero writes. Use
different fixtures than the implementer's upgrade tests.

## Review charter

Risk domain: data loss and compatibility breakage. Questions: (1) Does any
migration modify or delete existing user data? (2) Is the migration numbering
scheme collision-free and monotonic? (3) Are identity types used consistently
across all DTOs? (4) Can a migration failure leave the catalog in an
unrecoverable state? Severity: P0 for any data loss or unrecoverable state; P1
for any migration that silently changes user-visible behavior. Approval
vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Operation log or conflict semantics
- Server runtime or server control database
- Storage-root configuration UI
- Network sync
- Dropping legacy schema columns
- Changing current asset IDs, project IDs, paths, proxy paths, annotations, or clock behavior

## Stop conditions

Stop and escalate when:

- A migration would need to rewrite media files or generated caches
- An existing project ID or asset ID would need to change
- A legacy catalog cannot be upgraded without data loss
- The migration runner needs server crate or sync crate types

## Commands

```sh
cargo test -p rcdvlog-core
cargo test -p rcdvlog-cli
pnpm --dir apps/desktop check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Document assigned migration IDs and current schema version for later tasks. Do
not update broad product documentation yet (owned by S01-T020).
