# Task 7: Operation-Backed Project Mutations

**Status:** Implemented 2026-07-23
**Depends on:** Task 1
**Unblocks:** Tasks 8 and 15

## Objective

Convert current mutable project state to typed, immutable domain operations and deterministic materialized views while preserving desktop and CLI behavior.

## User outcome

Notes, labels, removal, rotation, clock corrections, and asset membership gain revision/conflict history needed for synchronization, but the existing desktop review flow remains fast and works entirely offline.

## Scope

- Add project operation log, origin sequence, hybrid logical clock, materialized revision, note revision, label-tag, and conflict tables.
- Implement node identity allocation for existing desktop installations and project catalogs.
- Add explicit Rust commands for note revision, label add/remove, removed state, rotation, project asset addition, and current fixed clock events.
- Route successful desktop ingest membership creation through `projectAssetAdded` operations while preserving per-file partial success.
- Materialize each operation transactionally and idempotently.
- Backfill current project state into a baseline/materialized revision without inventing user history.
- Adapt Tauri and CLI mutations to call the new commands.
- Preserve a compatibility `AssetPatch` adapter by calculating explicit changes and committing their operations atomically.
- Return canonical `Asset` projections after mutations.
- Add diagnostic projections for revision and conflict state.

## Out of scope

- Network transport or server replica
- Initial server baseline upload
- Web UI
- Web chronology editing
- Project or asset deletion
- New review statuses, favorites, trip context, scripts, or edit plans

## Operation rules

Each operation has a globally unique ID, project ID, origin node, monotonic origin sequence, hybrid logical time, aggregate type/ID, operation type, payload version, optional base revision, and typed payload.

Operations and their materialized effects commit together. Retrying an operation is a no-op. Local origin sequence allocation occurs in the same transaction as the mutation so crashes cannot acknowledge an unrecorded edit.

## Conflict behavior

- Notes create immutable revisions. The deterministic latest revision is active; all revisions remain queryable for diagnostics.
- Labels use an add-wins observed-remove set with normalized label values and unique add tags.
- `removed` and rotation use hybrid-logical last-write-wins registers.
- Rotation validation remains limited to 0, 90, 180, or 270 degrees.
- Clock changes carry base revision. Incompatible concurrent aggregate changes create a conflict record and do not silently replace active chronology.
- Asset addition is idempotent by project asset ID and cannot delete an asset.
- Server-assigned media linkage uses a distinct `projectAssetMediaLinked` operation so desktop ingest never invents a verified global identity.

## Design requirements

1. The Rust core is the only operation constructor/materializer for domain mutations.
2. Tauri, CLI, and future HTTP adapters cannot write operation/materialized tables directly.
3. HLC ordering is deterministic under equal or backward wall clocks and uses node ID as final tie-breaker.
4. Labels preserve current normalization and validation limits.
5. The full-patch compatibility path computes diffs against the current canonical asset and generates no operation for unchanged fields.
6. Multiple operations produced by one compatibility patch commit atomically.
7. Existing current-state rows are preserved and reconciled into materialized views during migration.
8. Operation payloads are versioned and reject unknown required fields/versions.
9. Operation history is append-only in the MVP.
10. Read-only catalog inspection reports pending migrations and conflicts without applying either.
11. Current optimistic desktop rollback behavior remains valid because commands return canonical state or fail atomically.
12. Clock materialization remains deterministic by camera and filename boundary.

## Likely implementation areas

- `core/rcdvlog-core` operation, HLC, materializer, command, and migration modules
- `apps/desktop/src-tauri/src/lib.rs` command adapters
- `apps/desktop/src/api.ts`, `types.ts`, and `App.tsx` only as required to preserve behavior
- `apps/cli/src/main.rs` explicit command mappings and diagnostics
- Core and CLI tests

## Acceptance criteria

- [x] Existing catalogs migrate and display identical active annotations and chronology before/after migration.
- [x] Every supported local mutation appends typed operations and updates materialized state in one transaction.
- [x] Each successfully ingested file has one idempotent `projectAssetAdded` operation, while a failed file does not roll back other successful imports.
- [x] Retrying the same operation ID does not duplicate revisions, label tags, or clock effects.
- [x] Concurrent note revisions both survive and choose the deterministic active revision.
- [x] Concurrent label add/remove tests demonstrate add-wins observed-remove behavior.
- [x] Removed and rotation registers converge under reversed application order.
- [x] Stale incompatible clock changes produce a conflict and do not silently change active chronology.
- [x] Unchanged full patches emit no new operations.
- [x] Multi-field compatibility patches either apply all changes or none.
- [x] Desktop keyboard/focus review behavior and optimistic rollback remain unchanged at the adapter boundary.
- [x] CLI success/error envelope rules and stable error codes remain intact.
- [x] Read-only commands do not allocate node IDs, sequences, or migrations.

## Verification

Run core property tests that shuffle operation delivery order and assert convergence. Run desktop type/build checks and CLI workflow tests. Exercise current labels, notes, removal, rotation, and clock flows manually in the browser mock and real Tauri app where native behavior is involved.

## Handoff notes

Freeze operation type names and payload version 1 before Task 8. Any later breaking semantic change requires a new operation/payload version rather than reinterpreting stored history.

The frozen v1 names and persistence contract are documented in [OPERATIONS.md](OPERATIONS.md). Automated browser and Rust gates pass; a real Tauri keyboard/focus smoke test was not run for this data-layer change.
