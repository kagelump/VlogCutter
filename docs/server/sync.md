# Project adoption, synchronization, and conflicts

The Mac and server keep independent local SQLite catalogs. Never copy a live catalog or WAL between them and never place either live database on NAS. Baseline format `1` creates the server replica; sync protocol `1.0` exchanges operations afterward.

## Baseline adoption

Desktop pairing first establishes a revocable device credential. Project adoption then builds a path-free baseline for one selected project. Preview validates counts and mappings without mutation. Confirmed import writes a temporary replica, verifies and replays operations, checkpoints it, and atomically registers it. Retrying the same digest reuses the replica; a changed digest cannot overwrite it.

The baseline carries project facts, immutable operations, unresolved conflicts, observations, and representation manifests. It carries no original/proxy bytes and no Mac absolute paths. Server-library assets added later become `projectAssetAdded` and `projectAssetMediaLinked` operations.

## Incremental reconciliation

Each request negotiates protocol/schema compatibility and carries per-origin cursors, at most 500 operations, and at most 4 MiB. Delivery is at-least-once. Origin sequences must be contiguous; duplicate sequence/operation pairs are idempotent, while a changed duplicate or gap reports attention and does not advance the cursor. Desktop uploads are paged in bounded 500-operation batches and server acknowledgements are persisted per peer.

The desktop commits local mutations before network I/O. One background loop per adopted project wakes after local changes, uses bounded retry/backoff, can fail over only between endpoints that prove the same server ID, treats WebSocket messages as hints, and recovers with HTTP polling/reconciliation. `sync now` accelerates but is not required for convergence.

Notes preserve revisions and choose a deterministic active value. Labels use add/remove operation semantics. Removed/rotation fields use deterministic last-writer materialization. Conflicting clock changes remain explicit and block unsafe chronology materialization. Protocol incompatibility or revoked credentials pauses sync without disabling local review.

## Restore interaction

A server restore can roll its operation allocator behind a Mac. The restored replica enters sequence recovery and emits no new server-origin operation until exact contiguous retained operations are replayed or a divergence is explicitly diagnosed. Do not force cursors or edit the database. Follow [backup-and-recovery.md](backup-and-recovery.md).

The protocol v1 schemas in [`docs/tasks/server/schemas`](../tasks/server/schemas/) are frozen compatibility artifacts. They are not generated-current files and must not be edited when implementation evolves; introduce a new version and compatibility path.

Use `rcdvlog server ... sync status|diagnose|now` for machine-readable state. The local `project diagnose` and `desktop last-opened` commands connect a screenshot breadcrumb to paired node, project, and conflict state.
