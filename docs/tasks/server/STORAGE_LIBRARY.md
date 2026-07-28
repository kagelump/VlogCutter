# Companion Storage and Library Foundation

> Historical Task 3 implementation snapshot. Current storage/library guidance is in [`docs/server/storage-and-library.md`](../../server/storage-and-library.md).

This is the implementation handoff for Task 3. It documents the Rust storage/library contracts consumed by later server tasks. Filesystem walking, hashing, media probing, jobs, transports, UI, synchronization, and representation storage remain owned by later tasks.

## Control-catalog migration

The control-catalog schema version is now `2`.

| ID | Name | Immutable source identity | Purpose |
| --- | --- | --- | --- |
| 1 | `control_catalog_foundation` | `server-control/0001-control-catalog-foundation/20260720` | Task 2 server identity, configuration revisions, project registry shell, and operational metadata. |
| 2 | `storage_global_library` | `server-control/0002-storage-global-library/20260720` | Logical storage roots, node-local bindings, global media/storage identity, rebuildable project membership, and scoped node observations. |

Migration 2 adds:

- `storage_roots`
- `node_root_bindings`
- `media_objects`
- `storage_objects`
- `media_object_locations`
- `project_media_memberships`
- `node_observation_sequences`
- `observation_generations`
- `node_asset_observations`

The migration adds indexes for root-relative lookup, verified content identity, project membership count, observation lookup, and deterministic library order. Its upgrade test begins at migration 1 and proves that the stable server identity and an existing project-registry row remain unchanged.

## Identity and ownership

`MediaObject` is project-independent content identity. `StorageObject` is one logical-root-relative physical file. `media_object_locations` associates those concepts, allowing multiple storage objects for one media object.

`project_media_memberships` is explicitly a rebuildable control-catalog projection keyed by `projectId` and `projectAssetId`, with the source operation ID that produced the projection. It contains no notes, labels, removal, rotation, chronology, or other annotation state. Project catalogs remain authoritative for project membership and annotations; a library insert never infers membership.

Global media verification states are:

- `unverified`
- `provisional`
- `verified`
- `mismatch`

Quick and full hashes are nullable. A `verified` media object requires both a 64-character lowercase hexadecimal full BLAKE3 digest and a size within SQLite's signed 64-bit range. The verified full hash has a partial unique index, so a second contradictory verified identity cannot be created. Location verification uses its own `unverified`, `provisional`, `verified`, and `mismatch` state.

## Logical roots and bindings

A `StorageRootId` identifies the logical root. The persisted binding row separates two contracts:

- `NodeRootCapability` is safe to synchronize for any node. It contains the node/root IDs, optional logical `rootSubpath`, protocol (`nfs`, `smb`, `local`, or `other`), access mode, enabled and verification state, association evidence, and explicit confirmation. It never contains an absolute path.
- `NodeRootBindingAdmin` is the current catalog node's local administrative projection. It adds that node's absolute `localMountPath` and is available only through explicitly local administrative methods.

`local_mount_path` is nullable in the control catalog. `upsert_node_root_capability` always inserts or updates a path-free capability and preserves any separately configured local path. Only `upsert_local_node_root_binding` can set a path, and it always owns the row for the catalog's stable server node. Remote capabilities, including a Mac capability used for observation availability, persist `NULL` for the path. JSON tests assert that sync-safe capability DTOs have no `localMountPath` property and do not contain the remote test path.

Absolute paths must never be copied into project state or synchronization contracts. Media, membership, observation, capability, and availability DTOs contain only stable IDs and root-relative paths. `StorageLibrary::resolve_local_storage_object` has no node parameter and loads only the current server node's non-null local binding. Distinct Mac and Linux paths therefore live only in their respective node-local catalogs; the server can still evaluate a Mac's path-free capability and observations.

An immutable original root accepts only a `read_only` binding. There is no domain API that creates, changes, renames, deletes, or places a marker in an original. Write resolution fails before filesystem access for an immutable root or read-only binding.

Root association evidence records counts for relative-path, quick-fingerprint, and full-hash matches, mismatch count, bounded sample relative paths, and capture time. Confirmation is a separate contract with `pending`, `confirmed`, or `rejected`; confirmed bindings require a confirming node and timestamp. Filesystem resolution and observation publication require an enabled root and an enabled, verified, explicitly confirmed binding.

## Root-relative paths

`RootRelativePath` is a shared protocol type serialized as a JSON string. Its canonical form:

- uses `/` as its only separator;
- is valid Unicode in NFC form and preserves spaces and non-ASCII characters;
- is non-empty and at most 4096 UTF-8 bytes;
- contains no empty, `.`, `..`, NUL, or other control components;
- is not Unix absolute, drive-qualified, UNC, backslash-separated, or mixed-separator;
- rejects percent escapes that could decode to traversal, separators, drive/absolute syntax, controls, or nested percent encoding, while allowing ordinary literal `%` filenames.

For a partial binding, the logical object must be at or below `rootSubpath`. Resolution strips that prefix before joining to the local mount. Existing mounts and candidates are canonicalized, and a candidate whose resolved path leaves the canonical mount through a symlink is rejected. Resolution never follows a remote node's stored administrative path.

## Observation generations

The observation scope key is `(nodeId, storageRootId, optional scopeRelativePath)`. `None` means the complete logical root. A partial binding cannot start a whole-root generation; its generation scope must equal or be nested under its `rootSubpath`.

Starting a generation increments one monotonic sequence for that node and inserts the `in_progress` generation in the same transaction. Observations are unique by generation and storage object, must belong to the same root and scope, and may be appended only while the generation is `in_progress`.

Completion is one short transaction that flips the generation to `completed` and supplies `completedAt`, thereby publishing all of its observations atomically. Repeating completion is idempotent. Abandoned generations reject completion. In-progress and abandoned generations never affect availability.

For a node and storage object, availability selects the highest **started node sequence** among completed generations whose scope contains the object. This remains deterministic across overlapping full-root and narrow generations even if an older generation completes later. If the winning completed generation omits the object, that node reports `missing_after_completed_observation`. A completed narrow generation affects only its subtree; another node or another scope is untouched.

## Availability projection

The public states are:

- `known`: media/location identity exists but no node binding covers it;
- `accessible_here`: the queried node's winning completed observation is accessible;
- `accessible_on_another_node`: the queried node is not accessible and another node is;
- `temporarily_unavailable`: the root/binding is disabled, unconfirmed, unavailable, or has no completed covering generation;
- `missing_after_completed_observation`: the winning completed scoped generation omitted the object;
- `fingerprint_mismatch`: the location link or winning node observation contradicts identity.

The aggregate is queried-node-aware. Precedence is: a location-level mismatch or queried-node mismatch, queried-node accessibility, another-node accessibility, another-node mismatch, missing, temporary unavailability, then known. Per-node/storage-object details and winning generation ID/sequence remain available even when the aggregate selects a different capability.

Disabling a root or binding changes only the availability projection. It preserves root identity, media, storage objects, membership projections, observations, and representations added by later tasks.

## Library pagination

`StorageLibrary::list_media` uses a bounded limit of 1–200 and no offset. Results sort by `(createdAt DESC, mediaObjectId ASC)`. The cursor carries exactly the last returned pair, and the continuation predicate/index matches that order. An optional verification-state filter uses a matching composite index. Each item includes storage-location and project-membership counts without exposing mount paths.

## Explicit non-capabilities

Task 3 does not walk a filesystem, create a scan, hash or probe media, expose a public route, synchronize project state, create representations, or run work in the background. Tests use only temporary directories and never access the real NAS fixture.
