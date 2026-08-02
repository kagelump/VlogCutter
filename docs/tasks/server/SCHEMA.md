# Companion Schema Foundation

> Historical Task 1 schema snapshot. Current schema identities are summarized in [`docs/server/README.md`](../../server/README.md).

This is the implementation handoff for the migration and identity contracts introduced by Task 1. Broad product documentation remains owned by Task 20.

## Project catalog migration contract

The current project-catalog schema version is `12`. Writable `Catalog::open` and `Catalog::open_existing` apply missing migrations in order. Read-only catalog opens and `catalog inspect` never create or update migration history.

| ID | Name | Immutable source identity | Purpose |
| --- | --- | --- | --- |
| 1 | `catalog_baseline` | `project-catalog/0001-catalog-baseline/20260720` | Records the existing projects, assets, clock, thumbnail, and batch-performance schema as the baseline. |
| 2 | `legacy_orientation_compatibility` | `project-catalog/0002-legacy-orientation-compatibility/20260720` | Adds and backfills orientation columns missing from older catalogs. |
| 3 | `legacy_annotation_compatibility` | `project-catalog/0003-legacy-annotation-compatibility/20260720` | Adds label and reversible-removal columns missing from older catalogs. |
| 4 | `companion_identity_links` | `project-catalog/0004-companion-identity-links/20260720` | Adds nullable project replica metadata and project-asset media identity state. |
| 5 | `operation_backed_project_mutations` | `project-catalog/0005-operation-backed-project-mutations/20260723` | Adds stable desktop/catalog identity, immutable project operations, HLC/sequence allocation, materialized revisions, note history, label tags, LWW registers, and chronology conflicts. |
| 6 | `project_sync_state` | `project-catalog/0006-project-sync-state/20260723` | Adds baseline identity, contiguous receive cursors, origin-scoped observation facts, and representation manifests. |
| 7 | `restore_sequence_recovery` | `project-catalog/0007-restore-sequence-recovery/20260728` | Adds durable restore/recovery state for server-origin operation sequence allocation. |
| 8 | `peer_operation_acknowledgements` | `project-catalog/0008-peer-operation-acknowledgements/20260728` | Adds per-peer committed cursors so sync acknowledges each origin's progress. |
| 9 | `chronology_facts` | `project-catalog/0009-chronology-facts/20260731` | Adds the persisted chronology fact model: `capture_sequences`, `asset_capture_sequence`, `clock_epochs`, `clip_time_exceptions`, `trip_phases`, and `event_order_constraints`, plus their ordering indexes. Existing `clock_events` rows are forward-migrated into `clock_epochs` without recomputing `assets.best_time` or `assets.offset_seconds`, so no existing review order changes. `clock_events` is retained for legacy operation replay. |
| 10 | `diagnostics` | `project-catalog/0010-diagnostics/20260803` | Adds the `diagnostics` table and its unacknowledged index (D01-T027). Purely additive: it creates one table and one index and touches no existing row. Diagnostics are durable *local* observations — deduplicated by `(project_id, dedupe_key)` with occurrence counting, acknowledged rather than deleted, and never typed operations, so they carry no HLC time, take no part in the revision guard, and are never synchronized to a peer. |
| 11 | `pattern_filename_counters` | `project-catalog/0011-pattern-filename-counters/20260803` | Backfills `asset_capture_sequence.filename_counter` where it is NULL and a camera filename pattern supplies one (D01-T028, `docs/tasks/desktop/CAMERA_PATTERNS.md`). It does not rewrite `assets.camera`, does not create or merge capture sequences, and does not touch `assets.best_time` or `assets.offset_seconds` — regrouping capture sequences would reorder an existing project. |
| 12 | `edit_plan` | `project-catalog/0012-edit-plan/20260803` | Adds the edit-plan model: `edit_plan_revisions`, `edit_plan_operations`, `edit_plan_exports`, and the partial unique index enforcing one active revision per project (D01-T019, `docs/tasks/desktop/EDIT_PLAN_MODEL.md`). `UNIQUE(revision_id, position)` makes two clips at one position unrepresentable. There is deliberately no `track` column, no `transition` table, and no `effect` column — not even reserved. |

Migration 9 freezes the desktop chronology model published in
`docs/tasks/desktop/CHRONOLOGY_MODEL.md` (D01-T007): capture sequences scope
filename identity, clock epochs carry one offset model per counter range, clip
time exceptions ride the `projectAsset` aggregate, trip phases carry timezone
context, and event-order constraints carry ordering knowledge independent of
time. Unknown operation types are retained in `project_operations` and reported
through the catalog's `quarantined_operations` surface instead of failing a
sync batch; a peer build that knows the type applies them normally from the
persisted log.

Every migration is committed in its own SQLite transaction. Its record is inserted into `schema_migrations` in that same transaction. Migration names and source identities must be non-empty and unique within a migration list. Applied names and source identities are immutable: changing either is treated as incompatible history. A migration list must be contiguous from ID 1, and a catalog with an applied version newer than the binary is read-only inspectable but rejected for writable use with `schema_incompatible` guidance.

Task 2 can reuse `SqliteMigration`, `run_sqlite_migrations`, and `inspect_sqlite_migrations` for the server control catalog while defining a separate control-catalog migration list and version.

## Added project fields

- `origin_node_id TEXT NULL`
- `paired_server_node_id TEXT NULL`
- `paired_at TEXT NULL`

Migration does not allocate a node, pair a server, or fabricate replica metadata. Those values remain `NULL` until their owning tasks make an explicit domain decision.

## Added project-asset fields

- `media_object_id TEXT NULL`
- `media_verification_state TEXT NOT NULL DEFAULT 'unlinked'`

`media_verification_state` is constrained to `unlinked`, `provisional`, `verified`, or `mismatch`. Existing and newly ingested desktop assets begin `unlinked`; a later server association operation owns transitions and assignment of `media_object_id`.

## Identifier and JSON contract

The Rust core exports distinct validated string types for node, project, project asset, media object, storage root, storage object, representation, operation, and job IDs. They serialize as plain JSON strings and remain opaque to adapters. Validation rejects empty IDs, IDs longer than 255 UTF-8 bytes, whitespace, and control characters. It does not reinterpret or rewrite existing project or asset ID strings.

DTO field names remain camelCase. Enum values introduced here use snake case, including `mediaVerificationState` values and catalog `schemaStatus` values. `catalog inspect` reports the recorded version separately from the latest supported version and lists both applied and pending migrations.

Migration 5 reconciles existing annotations and memberships as revision-zero baselines. It does not fabricate operations or user history. Existing fixed clock events establish each camera aggregate's starting revision. See [OPERATIONS.md](OPERATIONS.md).

Migration 6 adds synchronization bookkeeping without copying local paths into the portable project state. The control catalog independently advances to version 6 with replica digest/identity fields and the repairable project-membership projection queue. See [SYNC_PROTOCOL.md](SYNC_PROTOCOL.md).
