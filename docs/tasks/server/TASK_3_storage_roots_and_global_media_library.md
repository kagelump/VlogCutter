# Task 3: Storage Roots and Global Media Library

**Status:** Implemented; physical NAS acceptance remains external
**Depends on:** Tasks 1 and 2
**Unblocks:** Tasks 5, 8, 10, 14, and 16

## Objective

Implement the server-wide identity model for logical storage roots, node-local bindings, global media objects, physical storage objects, project membership links, and node-relative availability.

## User outcome

The server can represent “Main NAS Video” once even when the Mac and Ubuntu server mount it at different absolute paths, and it can maintain a global media library without confusing media identity with project annotations.

## Scope

- Add server control-catalog migrations for storage and global media identity.
- Implement storage-root create/list/update-disable operations.
- Implement server `NodeRootBinding` with local mount path, optional root subpath, protocol, access mode, and verification status.
- Define project-independent `MediaObject` and root-relative `StorageObject` records.
- Define project membership linkage without moving project annotations into the global library.
- Add safe root-relative path parsing and resolution.
- Add generation-based node availability observations.
- Add explicit root-association evidence and confirmation data contracts for later desktop/web UI.
- Add paginated library query primitives and indexes.

## Out of scope

- Walking/scanning the filesystem
- Fingerprint or metadata jobs
- Root-association UI
- Project synchronization
- Representation storage
- Original relocation or marker-file creation

## Core model

At minimum, persist:

- `storage_roots`
- `node_root_bindings`
- `media_objects`
- `storage_objects`
- `media_object_locations`
- `project_media_memberships` or an equivalent server-side projection
- `observation_generations`
- `node_asset_observations`

The exact normalized schema may vary, but it must preserve the distinctions in `ARCHITECTURE.md`.

## Design requirements

1. `StorageRootId` is logical and stable; mount paths are binding-local.
2. A binding supports `rootSubpath` so a node may mount only a subtree.
3. Original roots are marked immutable independently of filesystem permissions.
4. The application never writes `.rcdvlog-storage-root` or any marker into an original tree.
5. A root-relative path cannot escape its binding through traversal, alternate separators, symlinks, or encoding tricks.
6. Availability distinguishes known, accessible here, accessible on another node, temporarily unavailable, missing after a completed observation generation, and fingerprint mismatch.
7. An incomplete scan generation cannot mark prior observations missing.
8. Global media records allow nullable quick/full hashes and explicit verification states.
9. A `ProjectAssetId` remains project-scoped and may link to one global `MediaObjectId`; annotations never appear in global media tables.
10. The server library supports one media object in multiple projects and multiple storage objects for one media object.
11. Absolute mount paths appear only in local administrative/diagnostic projections, never in project sync payloads as canonical paths.
12. Disabling a root preserves historical identity and project links.

## Likely implementation areas

- `core/rcdvlog-core` shared identity and availability DTOs
- `core/rcdvlog-server` control-catalog migrations, repositories, and services
- `apps/server` internal administrative routes only if needed for tests; public auth arrives in Task 9
- Test fixtures for path normalization and partial-root resolution

## Acceptance criteria

- [ ] The same `storageRootId` resolves through distinct Mac and Linux bindings.
- [ ] A partial binding resolves assets under `rootSubpath` and rejects assets outside it.
- [ ] Traversal, absolute, symlink-escape, and malformed relative paths are rejected.
- [ ] Two storage objects may point to one media object without becoming two project assets automatically.
- [ ] Two project assets may reference one media object while retaining independent annotations.
- [ ] A completed new observation generation supersedes only the originating node and scanned scope.
- [ ] An interrupted generation leaves prior availability intact.
- [ ] Disabling or unmounting a binding changes availability without deleting media, project, or representation records.
- [ ] Library queries are cursor-paginated and deterministic.
- [ ] No test creates a marker or writes into an original root.
- [ ] Control-catalog migration upgrades preserve server identity and project registry state.

## Verification

Run server-core tests plus property tests over relative-path parsing and resolution. Include paths with spaces, non-ASCII characters, mixed separators, `..`, symlinks, and partial mounts.

Use temporary roots only. Verify an original root made filesystem-read-only still supports observation reads while every write attempt through the domain service fails.

## Handoff notes

Expose library/storage services through Rust interfaces consumed by Tasks 5, 8, 10, 14, and 16. Do not make transport DTOs the internal persistence model.
