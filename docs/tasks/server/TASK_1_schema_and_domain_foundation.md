# Task 1: Schema and Domain Foundation

**Status:** Implemented
**Depends on:** None
**Unblocks:** Tasks 2, 3, and 7

## Objective

Introduce the versioned migration and shared identity foundations required by the companion server without changing current desktop behavior or losing compatibility with existing project catalogs.

## User outcome

Existing projects open exactly as before, but their catalogs can now evolve safely through explicit forward migrations and carry the stable identity fields needed for later server pairing.

## Scope

- Replace implicit `CREATE TABLE IF NOT EXISTS` evolution with numbered, transactional forward migrations.
- Preserve `Catalog::open_existing` validation and read-only inspection behavior.
- Add strongly defined shared identifiers for node, project, project asset, media object, storage root, storage object, representation, operation, and job identities.
- Add project-level replica metadata needed to record a future paired server without pairing anything yet.
- Add nullable project-asset linkage fields for future global media identity and verification state.
- Establish serialization, validation, and error conventions shared by desktop, CLI, sync, and server code.
- Record current schema as the migration baseline and add upgrade fixtures.

## Out of scope

- Operation log or conflict semantics
- Server runtime or server control database
- Storage-root configuration UI
- Network sync
- Changing current asset IDs, project IDs, paths, proxy paths, annotations, or clock behavior
- Dropping legacy schema columns

## Design requirements

1. Add a `schema_migrations` table with monotonically ordered migration IDs, names, checksums or immutable source identity, and applied timestamps.
2. A migration runs once inside a transaction. Failure leaves the prior schema and user data usable.
3. `Catalog::open` and writable `open_existing` migrate forward; read-only opens and diagnostics never migrate.
4. The migration runner must reject a catalog containing an unknown newer schema rather than attempting a downgrade.
5. Existing idempotent table creation becomes the baseline migration rather than being removed without replacement.
6. Existing older catalogs, including those missing newer orientation/label/removal columns, must upgrade through tested steps.
7. IDs remain opaque strings at storage and JSON boundaries. New Rust types may use validated newtypes but must preserve current JSON values.
8. Add nullable linkage rather than fabricating a `mediaObjectId` before a server has verified or assigned it.
9. Add a verification enum capable of at least `unlinked`, `provisional`, `verified`, and `mismatch`.
10. Every DTO continues to use camelCase unless an existing public enum contract says otherwise.
11. Migrations may add indexes but must not rewrite media files or generated caches.

## Likely implementation areas

- `core/rcdvlog-core/src/` migration, identifier, and catalog modules
- `core/rcdvlog-core/src/lib.rs` compatibility exports
- `apps/desktop/src/types.ts` only for new fields actually returned during this task
- `apps/cli/src/main.rs` catalog inspection schema fields
- `apps/cli/tests/cli_workflow.rs`
- New older-schema fixtures under a test-only fixture directory

Avoid splitting the whole current Rust file solely for aesthetics. Extract only seams required to make migrations and identifiers testable.

## Acceptance criteria

- [ ] A current catalog upgrades once and reports the expected schema version.
- [ ] Reopening the upgraded catalog does not rerun migrations or change data.
- [ ] A pre-orientation/pre-label fixture upgrades without losing projects, assets, notes, labels, removal state, clock events, thumbnails, or proxy references.
- [ ] A deliberately failing migration rolls back completely in a temporary test catalog.
- [ ] A catalog with a newer unsupported schema fails with a stable actionable error.
- [ ] `open_read_only_existing`, `doctor`, and catalog inspection do not create `schema_migrations` or modify the file.
- [ ] Existing `Project`, `Asset`, `AssetPatch`, ingest, proxy, thumbnail, and chronology tests continue to pass.
- [ ] Existing project and asset ID strings are unchanged.
- [ ] New nullable media-link fields serialize predictably and do not make old browser mocks invalid.
- [ ] No source or cache file is read or written as part of schema migration.

## Verification

Run:

```bash
cargo test -p rcdvlog-core
cargo test -p rcdvlog-cli
pnpm --dir apps/desktop check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

Inspect an upgraded temporary catalog through the CLI and confirm the JSON distinguishes current schema version from pending migrations.

## Handoff notes

Document the assigned migration IDs and current schema version in code comments or a focused schema document consumed by later tasks. Do not update the broad product documentation yet; Task 20 owns the final repository-wide documentation audit.
