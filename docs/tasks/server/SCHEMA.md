# Companion Schema Foundation

> Historical Task 1 schema snapshot. Current schema identities are summarized in [`docs/server/README.md`](../../server/README.md).

This is the implementation handoff for the migration and identity contracts introduced by Task 1. Broad product documentation remains owned by Task 20.

## Project catalog migration contract

The current project-catalog schema version is `6`. Writable `Catalog::open` and `Catalog::open_existing` apply missing migrations in order. Read-only catalog opens and `catalog inspect` never create or update migration history.

| ID | Name | Immutable source identity | Purpose |
| --- | --- | --- | --- |
| 1 | `catalog_baseline` | `project-catalog/0001-catalog-baseline/20260720` | Records the existing projects, assets, clock, thumbnail, and batch-performance schema as the baseline. |
| 2 | `legacy_orientation_compatibility` | `project-catalog/0002-legacy-orientation-compatibility/20260720` | Adds and backfills orientation columns missing from older catalogs. |
| 3 | `legacy_annotation_compatibility` | `project-catalog/0003-legacy-annotation-compatibility/20260720` | Adds label and reversible-removal columns missing from older catalogs. |
| 4 | `companion_identity_links` | `project-catalog/0004-companion-identity-links/20260720` | Adds nullable project replica metadata and project-asset media identity state. |
| 5 | `operation_backed_project_mutations` | `project-catalog/0005-operation-backed-project-mutations/20260723` | Adds stable desktop/catalog identity, immutable project operations, HLC/sequence allocation, materialized revisions, note history, label tags, LWW registers, and chronology conflicts. |
| 6 | `project_sync_state` | `project-catalog/0006-project-sync-state/20260723` | Adds baseline identity, contiguous receive cursors, origin-scoped observation facts, and representation manifests. |

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
