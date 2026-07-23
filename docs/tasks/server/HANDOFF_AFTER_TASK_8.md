# Handoff After Task 8

**Date:** 2026-07-23  
**Status:** Task 8 implemented; automated Rust and HTTP gates pass  
**Next:** Task 9 authentication, then Tasks 10–11 pairing and desktop sync

## Frozen identities

- Sync protocol: **1.0**
- Baseline format: **1**
- Project catalog schema: **6**
- Control catalog schema: **6**
- Project migration 6: `project_sync_state`
- Control migration 6: `project_replica_sync_registry`

The v1 JSON contracts are committed under `docs/tasks/server/schemas/`.

## Public implementation surface

- `rcdvlog-sync`: baseline export/digest/validation, protocol negotiation, bounded reconciliation, operation/fact cursors, and stable compatibility errors.
- `Catalog::export_project_baseline`, `Catalog::import_project_baseline`, `Catalog::apply_operation_batch`, `Catalog::operation_cursors`, and `Catalog::pair_project_with_server`.
- `ProjectReplicaManager`: read-only preview, crash-safe import, idempotent registration, replica open/reconcile, server-origin asset add/media link, and membership-projection repair.
- Internal HTTP:
  - `POST /api/v1/internal/projects/baseline/preview`
  - `POST /api/v1/internal/projects/baseline/import`
  - `POST /api/v1/internal/projects/{projectId}/reconcile`

## Constraints for follow-on tasks

1. Authenticate the existing internal routes in Task 9; do not fork the DTOs.
2. Pairing must persist the returned stable server node ID before background sync.
3. Use per-origin cursors and retry pages verbatim after response loss.
4. Never send absolute mount paths, live SQLite files, WAL files, or transient job progress.
5. Preserve the stable `projectAssetId` when a global media association arrives.
6. Treat control-catalog membership as a repairable projection, not an authority or cross-database transaction.

## Verification

Deterministic tests cover selected-project/path-free baseline export, equivalent import, same-digest retry, different-digest rejection, failed temporary import cleanup, offline two-replica convergence, duplicate delivery, sequence gaps, origin-scoped observations, manifest/job-progress separation, server HTTP protocol errors, and crash-boundary membership repair.
