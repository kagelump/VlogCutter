# Project Operation Contract

> Historical Task 7 implementation snapshot. Current synchronization guidance is in [`docs/server/sync.md`](../../server/sync.md).

This is the Task 7 handoff for immutable project mutations. Payload version 1 and the operation type names below are frozen for Task 8 synchronization.

## Envelope and ordering

Every operation stores `operationId`, `projectId`, `originNodeId`, monotonic `originSequence`, hybrid logical time (`wallMillis`, `counter`, `nodeId`), `aggregateType`, `aggregateId`, `operationType`, `payloadVersion`, optional `baseRevision`, and a typed JSON payload. `(originNodeId, originSequence)` and `operationId` are unique. The operation and its materialized effect commit in one immediate SQLite transaction; retrying an existing operation ID returns `duplicate` without another effect.

Desktop settings persist one stable installation node ID. Each opened or created catalog records that node before its first mutation. CLI-only catalogs allocate a stable catalog-local desktop node on the first real mutation. Unchanged compatibility patches and all read-only inspection paths allocate nothing.

HLC comparison is `(wallMillis, counter, nodeId, operationId)`. The local clock advances monotonically under equal or backward wall time and incorporates received timestamps before the next local event.

## Version 1 types

| Operation type | Aggregate | Payload | Materialization |
| --- | --- | --- | --- |
| `projectAssetAdded` | `projectAsset` | Project-asset ID, sampled fingerprint, filename, technical/camera/timestamp/orientation facts, creation time | Idempotent portable membership; never deletes an asset. |
| `projectAssetMediaLinked` | `projectAsset` | `mediaObjectId`, `verificationState` | Server-owned global-media link; desktop ingest never creates it. |
| `noteRevisionCreated` | `projectAsset` | `revisionId`, `text` | Immutable note revision; greatest HLC key is active and all revisions remain diagnostic. |
| `labelAdded` | `projectAsset` | `addTag`, `label` | Adds one normalized label tag. |
| `labelRemoved` | `projectAsset` | `normalizedLabel`, `observedAddTags` | Removes only observed tags, so an unobserved concurrent add wins. |
| `removedSet` | `projectAsset` | `removed` | HLC last-write-wins register. |
| `rotationSet` | `projectAsset` | `rotationDegrees` | HLC last-write-wins register; only 0, 90, 180, and 270 are valid. |
| `clockEventCreated` | `clock` keyed by camera | Event ID, camera, filename boundary, offset seconds, label | Requires the current camera base revision. A stale base records an unresolved conflict and leaves chronology unchanged. |

Payload structs reject unknown fields and any payload version other than 1. Breaking semantics require a new operation type or payload version.

Absolute source paths are excluded from `projectAssetAdded`. Desktop ingest commits its path separately to `project_asset_local_locations` under the local node ID in the same transaction; a remote replica materializes the portable asset as original-offline until it has its own node-local location.

## Compatibility and diagnostics

`Catalog::update_asset` remains the full-patch adapter used by desktop and CLI. It normalizes labels, diffs canonical state, and commits all resulting explicit operations atomically. Invalid fields roll back the whole patch; unchanged fields emit no operation. Explicit core/Tauri commands also exist for note revision, label add/remove, removal, and rotation.

`Catalog::list_operations`, `operation_diagnostics`, and `asset_revision_diagnostics` provide Task 8 and CLI-facing projections. `catalog inspect` reports unresolved conflict count without migrating or allocating identity. Project and clip diagnoses include operation/conflict and note-revision state.
