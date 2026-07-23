# Sync Protocol v1

Task 8 freezes baseline version 1 and sync protocol `1.0`. The transport DTOs live in `core/rcdvlog-sync`; HTTP is only one adapter.

## Baseline

`BaselineEnvelope` contains a BLAKE3 digest of the serialized `BaselinePayload`. It includes exactly one selected project, path-free static asset facts and revision-zero state, clock baselines, immutable project operations, unresolved conflicts, node observations, and representation manifests.

Preview validates the envelope and reports counts without writing either catalog. Import writes a temporary project catalog under the server data directory, replays operations through the core materializer, validates the selected project, checkpoints WAL, atomically renames the database, and then registers it. A retry with the same digest reuses the replica. A different digest returns `baseline_different` and never overwrites the existing file.

## Reconciliation

Each request identifies the project and paired server, supplies a compatible protocol range and project schema version, reports the sender's per-origin operation cursors, and carries bounded operation/fact pages. The current limits are 500 operations and 4 MiB per request.

Operations are grouped by origin, sorted by origin sequence, and committed only when they extend the stored contiguous cursor. Repeated acknowledged operations must have the same operation ID. A gap or changed duplicate returns `sync_sequence_gap` without advancing the cursor.

The response returns:

- the negotiated protocol version;
- committed receive cursors;
- operations missing after the requester's reported cursors;
- observation and representation-manifest cursors;
- unresolved project conflicts;
- `hasMoreOperations` for bounded pull pagination.

Observations are immutable facts keyed by origin sequence. A later observation supersedes only the same origin and scope when consumers select the latest sequence. Representation manifests use an independent stream and reject job-progress fields; durable representation identity is synchronized, transient execution progress is not.

## Replica and projection ownership

Each registered project has one SQLite replica under a digest-derived relative filename in `<dataDir>/projects`. Absolute client paths never enter a baseline or reconciliation DTO. The stable server node ID constructs server-origin project operations.

`projectAssetAdded` is authoritative project membership. `projectAssetMediaLinked` associates the stable project asset with a verified or provisional global media object. The control-catalog `project_media_memberships` row is a replayable projection. If the process stops after the project commit, `ProjectReplicaManager::repair_membership_projections` reconstructs it idempotently from the source operation.

## Compatibility errors

- `sync_protocol_incompatible`
- `sync_schema_incompatible`
- `sync_project_mismatch`
- `sync_server_mismatch`
- `sync_page_too_large`
- `sync_sequence_gap`
- `baseline_digest_mismatch`
- `baseline_version_incompatible`
- `baseline_different`

The frozen JSON schemas are:

- [baseline-v1.schema.json](schemas/baseline-v1.schema.json)
- [reconcile-v1.schema.json](schemas/reconcile-v1.schema.json)
- [reconcile-response-v1.schema.json](schemas/reconcile-response-v1.schema.json)

Task 9 adds authentication to the internal HTTP routes. Tasks 10 and 11 should consume these envelopes rather than opening or transferring live SQLite/WAL files.
