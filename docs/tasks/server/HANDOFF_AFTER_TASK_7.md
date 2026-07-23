# Handoff After Task 7

**Date:** 2026-07-23
**Status:** Task 7 implemented; automated gates pass; real Tauri keyboard/focus smoke test not run
**Next:** Task 8 — Sync Protocol and Server Project Replicas

## Migration identity

Project catalog schema version: **5**

Migration 5 is `operation_backed_project_mutations` with immutable source identity `project-catalog/0005-operation-backed-project-mutations/20260723`. It preserves visible annotations and chronology, reconciles existing state into revision-zero baselines, and does not invent historical operations.

## Public core surface

- `ProjectOperation`, `HybridLogicalTime`, and `ApplyOperationResult`
- `Catalog::apply_operation` and `Catalog::list_operations`
- Explicit note, label, removed, and rotation commands
- Compatibility `Catalog::update_asset` backed by atomic explicit operations
- `Catalog::operation_diagnostics` and `Catalog::asset_revision_diagnostics`
- Conflict, note-revision, and materialized-revision projections

The v1 operation names and payload semantics are frozen in [OPERATIONS.md](OPERATIONS.md).

## Task 8 starting constraints

1. Use `ProjectOperation` envelopes; do not synchronize SQLite files or reinterpret payload version 1.
2. Validate contiguous per-origin sequences in the sync layer before acknowledging a cursor. The core enforces uniqueness but intentionally does not own transport cursors.
3. Apply received operations only through `Catalog::apply_operation`.
4. `projectAssetAdded` is authoritative membership. Control-catalog membership remains a repairable projection.
5. Server media association must use `projectAssetMediaLinked`; never replace the project asset ID.
6. Preserve unresolved clock conflicts in baseline and reconciliation responses.
7. Baseline export/import remains Task 8 work; Task 7 provides operation and diagnostic primitives, not replica registration or transport.

## Verification completed

- Operation unit/property tests cover duplicate delivery, reversed note/LWW delivery, shuffled rotation delivery, observed-remove add-wins labels, atomic full patches, unchanged patches, and stale clock conflicts.
- Existing media integration asserts one `projectAssetAdded` operation per successful ingest.
- `pnpm check`, frontend type checking, and workspace tests pass at handoff time.

## Manual gap

No real native Tauri smoke test was run. The React review behavior was not changed; the existing full-patch return/rollback contract remains intact.
