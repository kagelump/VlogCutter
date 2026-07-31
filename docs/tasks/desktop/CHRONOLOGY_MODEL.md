# Chronology model — frozen contract

Published by D01-T007 (`docs/tasks/desktop/tasks/D01-T007-chronology-domain-model.md`),
2026-07-31. This document freezes the persisted chronology fact model, the
operation vocabulary, and the migration rules for D01-T008 through D01-T017.
Every packet in that range builds on this document and may not change it; a
change to any contract below is a new task with a new migration.

## Status

- Project catalog schema: 9 (migration 9, `chronology_facts`).
- `docs/tasks/server/SCHEMA.md` is updated to match.
- D01-T008 through D01-T017 are `draft` until this document is published; the
  contract editor promotes them and reconciles their drafts with what is frozen
  here.

## Required interface

### Tables, migration 9

```sql
CREATE TABLE IF NOT EXISTS capture_sequences (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    camera_key TEXT NOT NULL,
    label TEXT NOT NULL,
    volume_hint TEXT NOT NULL DEFAULT '',
    filename_prefix TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS asset_capture_sequence (
    asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    capture_sequence_id TEXT NOT NULL REFERENCES capture_sequences(id) ON DELETE CASCADE,
    filename_counter INTEGER
);

CREATE TABLE IF NOT EXISTS clock_epochs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    capture_sequence_id TEXT NOT NULL REFERENCES capture_sequences(id) ON DELETE CASCADE,
    start_counter INTEGER NOT NULL,
    end_counter INTEGER,
    offset_seconds INTEGER NOT NULL,
    confidence REAL NOT NULL DEFAULT 1.0,
    evidence_json TEXT NOT NULL DEFAULT '[]',
    label TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS clip_time_exceptions (
    asset_id TEXT PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    trip_time TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS trip_phases (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    label TEXT NOT NULL,
    phase_kind TEXT NOT NULL CHECK(phase_kind IN ('location', 'transit')),
    start_trip_time TEXT NOT NULL,
    end_trip_time TEXT NOT NULL,
    timezone TEXT,
    origin_timezone TEXT,
    destination_timezone TEXT,
    display_mode TEXT NOT NULL CHECK(display_mode IN ('local', 'dual', 'utc')),
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS event_order_constraints (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    earlier_asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    later_asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    strength TEXT NOT NULL CHECK(strength IN ('hard', 'soft')),
    source TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(project_id, earlier_asset_id, later_asset_id)
);

CREATE INDEX IF NOT EXISTS clock_epochs_sequence_range
ON clock_epochs(project_id, capture_sequence_id, start_counter);

CREATE INDEX IF NOT EXISTS trip_phases_project_range
ON trip_phases(project_id, start_trip_time);
```

There is deliberately no `clock_anchors` table. `PRD.md` §11 anchors depend on
automatic visual clock detection, which §21.3 defers past the MVP. A
user-confirmed anchor is expressible as a clip time exception plus evidence, and
adding an unused table now would be reserved schema for deferred work.

### Operation vocabulary

```rust
pub const CAPTURE_SEQUENCE_DECLARED: &str = "captureSequenceDeclared";
pub const CAPTURE_SEQUENCE_SPLIT: &str = "captureSequenceSplit";
pub const CAPTURE_SEQUENCE_MERGED: &str = "captureSequenceMerged";
pub const CLOCK_EPOCH_SET: &str = "clockEpochSet";
pub const CLOCK_EPOCH_CLEARED: &str = "clockEpochCleared";
pub const CLIP_TIME_EXCEPTION_SET: &str = "clipTimeExceptionSet";
pub const CLIP_TIME_EXCEPTION_CLEARED: &str = "clipTimeExceptionCleared";
pub const TRIP_PHASE_SET: &str = "tripPhaseSet";
pub const TRIP_PHASE_REMOVED: &str = "tripPhaseRemoved";
pub const EVENT_ORDER_CONSTRAINT_SET: &str = "eventOrderConstraintSet";
pub const EVENT_ORDER_CONSTRAINT_CLEARED: &str = "eventOrderConstraintCleared";
```

| Operation | `aggregate_type` | `aggregate_id` |
| --- | --- | --- |
| `captureSequence*` | `captureSequence` | capture sequence id |
| `clockEpoch*` | `clockEpoch` | clock epoch id |
| `clipTimeException*` | `projectAsset` | asset id |
| `tripPhase*` | `tripPhase` | trip phase id |
| `eventOrderConstraint*` | `eventOrderConstraint` | constraint id |

`CLOCK_EVENT_CREATED` remains a recognized operation type forever, materializing
into `clock_epochs` through the same path migration 9 uses. Replaying an
operation log that predates this change must still work; that log is the
authority for existing projects and cannot be rewritten.

### Rust types

```rust
// core/rcdvlog-core/src/chronology/model.rs
pub struct CaptureSequence {
    pub id: String,
    pub camera_key: String,
    pub label: String,
    pub volume_hint: String,
    pub filename_prefix: String,
}

pub struct AssetTimeHypothesis {
    pub asset_id: String,
    pub filename: String,
    pub captured_at_raw: Option<String>,
    pub capture_sequence_id: Option<String>,
    pub filename_counter: Option<u64>,
    pub duration_seconds: f64,
}
```

The camera-metadata timezone (`PRD.md` §9.1, optional) is deliberately not
persisted. Local display time derives from the covering trip phase, never from
camera metadata timezone (architecture contract §3.4), and the solver never
reads camera metadata timezones; a stored column would be reserved schema with
no consumer. This is a documented exclusion decision, not an omission for a
later packet to repair.

pub struct ClockEpoch {
    pub id: String,
    pub capture_sequence_id: String,
    pub start_counter: u64,
    pub end_counter: Option<u64>,
    pub offset_seconds: i64,
    pub confidence: f32,
    pub evidence: Vec<String>,
    pub label: String,
}

pub struct ClipTimeException {
    pub asset_id: String,
    pub trip_time: String,
    pub reason: String,
}

pub enum PhaseKind { Location, Transit }
pub enum DisplayMode { Local, Dual, Utc }

pub struct TripPhase {
    pub id: String,
    pub label: String,
    pub kind: PhaseKind,
    pub start_trip_time: String,
    pub end_trip_time: String,
    pub timezone: Option<String>,
    pub origin_timezone: Option<String>,
    pub destination_timezone: Option<String>,
    pub display_mode: DisplayMode,
}

pub enum ConstraintStrength { Hard, Soft }

pub struct EventOrderConstraint {
    pub id: String,
    pub earlier_asset_id: String,
    pub later_asset_id: String,
    pub strength: ConstraintStrength,
    pub source: String,
}

/// The complete persisted evidence for one project. Every collection is
/// returned in a deterministic order, sorted by primary key, so the solver
/// cannot inherit nondeterminism from the loader.
pub struct ChronologyFacts {
    pub project_id: String,
    pub assets: Vec<AssetTimeHypothesis>,
    pub sequences: Vec<CaptureSequence>,
    pub epochs: Vec<ClockEpoch>,
    pub exceptions: Vec<ClipTimeException>,
    pub phases: Vec<TripPhase>,
    pub constraints: Vec<EventOrderConstraint>,
}
```

```rust
// core/rcdvlog-core/src/lib.rs, additive
impl ProjectCatalog {
    pub fn chronology_facts(&self, project_id: &str) -> Result<ChronologyFacts>;
    pub fn quarantined_operations(&self, project_id: &str) -> Result<Vec<QuarantinedOperation>>;
}

pub struct QuarantinedOperation {
    pub operation_id: String,
    pub operation_type: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub payload_version: u32,
    pub received_at: String,
}
```

The concrete type is `Catalog` (the implementation type for `ProjectCatalog` in
this codebase). All collections inside `ChronologyFacts` are sorted by primary
key: assets by `assets.id`, sequences by `capture_sequences.id`, epochs by
`clock_epochs.id`, exceptions by `clip_time_exceptions.asset_id`, phases by
`trip_phases.id`, constraints by `event_order_constraints.id`.

### Migration 9 forward-migration rule

For each project:

1. Group assets by `assets.camera`. Each distinct camera becomes one
   `capture_sequences` row with `camera_key` set to the camera string,
   `label` set to the camera string, and empty `volume_hint`. Migration does not
   attempt to detect card boundaries; splitting is D01-T009's user-driven work,
   and guessing here would reorder existing projects.
2. For each asset, insert `asset_capture_sequence` with its sequence and its
   `filename_counter` parsed from the filename's trailing digit run. A filename
   with no digit run gets a NULL counter.
3. For each `clock_events` row, insert a `clock_epochs` row with
   `capture_sequence_id` set to that camera's sequence, `start_counter` set to
   the counter parsed from `start_filename`, `end_counter` NULL,
   `offset_seconds` carried over, `confidence` 1.0, `evidence_json`
   `["migrated_clock_event"]`, and `label` carried over.
4. Where `start_filename` has no parseable counter, `start_counter` is 0, which
   preserves today's behavior of applying the offset to the whole camera.
5. `clock_events` is retained, not dropped. Operation replay still targets it and
   a rollback must remain possible. It stops being read by ordering queries.
6. `assets.best_time` and `assets.offset_seconds` are left exactly as they are.
   Migration does not recompute them, because recomputation is precisely how a
   migration reorders a project.

Implementation detail: a camera present only in `clock_events` (no assets carry
it) also receives a `capture_sequences` row, so its epochs can be carried over
without violating the foreign key. This adds no asset rows and therefore cannot
change any review order.

## Frozen interface

Everything in the Required Interface section above is frozen. This section is
the rest of the contract the D01-T008 through D01-T017 packets need to build
without further design.

### Evidence only; projections are off-limits

Persisted chronology state is evidence. Trip time, local display time,
confidence, evidence, uncertainty, and total order are derived by the pure
solver (D01-T008). `assets.best_time` and `assets.offset_seconds` are
projections of the solution for query performance: no new chronology
operation materializer and no migration writes them, and no materializer
introduced after this contract may write them. The legacy `CLOCK_EVENT_CREATED`
materializer still invokes the pre-existing `recalculate_clock` helper, which
writes both columns; D01-T008 replaces that path. After D01-T008, the solver's
projection helper is the sole writer of both columns. D01-T009 through D01-T017
must never write them.

### Capture sequences

- Filename identity is scoped to the capture sequence (`PRD.md` §8.2): two
  cards legitimately both contain `C0001.MP4`.
- Every asset belongs to exactly one sequence at all times. Migration 9 assigns
  every existing asset; a new asset created by `projectAssetAdded` is assigned
  to its camera's sequence, creating the sequence on demand with the
  migration-9 shape (`camera_key` and `label` are the camera string). There is
  no unassigned state.
- The sequence id for one project camera is derived deterministically:
  `sequence_<project_id>_<blake3 hex of the camera string>`. Migration 9 and
  the operation materializers share this derivation, so legacy replay and
  migration produce identical epochs. The digest keeps camera strings of any
  shape out of the id (operation aggregate ids must be whitespace-free).
- `filename_counter` is the trailing digit run of the filename's stem
  (extension stripped, trailing non-alphanumeric characters stripped):
  `C0101.MP4` → 101, `DJI_0042.MP4` → 42, `clip.mp4` → NULL,
  `GX010234.MP4` → 10234, `IMG_1234 (1).MOV` → 1. A digit run exceeding `u64`
  yields NULL, never a panic.
- A split moves assets with `filename_counter >= at_counter` into a new
  sequence; assets with a NULL counter stay in the source sequence. A merge
  unions the asset sets into the destination sequence, deletes the source
  sequence, and re-points the source sequence's epochs at the destination
  (merging unions the facts). Neither ever drops an asset.

### Clock epochs

- A range is `[start_counter, end_counter]` inclusive; a NULL `end_counter`
  means open-ended (`D01-T010` frozen decision).
- An epoch applies only within its own capture sequence. It cannot reach across
  sequences, even for the same camera.
- An asset with a NULL counter is never inside any bounded range; it is
  affected only by an open-ended epoch on its sequence.
- Epochs never modify `captured_at_raw` and never modify source metadata.
- Clearing an epoch restores whatever the remaining epochs imply; it does not
  restore a snapshot.
- `confidence` defaults to 1.0 and `evidence` to an empty list when the payload
  omits them. Evidence is an ordered list of strings.

### Clip time exceptions

- A clip time exception is a single-clip override that breaks its capture
  sequence (`PRD.md` §9.4). It rides the `projectAsset` aggregate so it inherits
  that aggregate's revision guard; its identity is the asset id. There is one
  exception row per asset; setting replaces, clearing removes.
- `trip_time` is an RFC 3339 UTC string.

### Trip phases

- A phase is `location` or `transit` (`PhaseKind`) with `local`, `dual`, or
  `utc` display mode (`DisplayMode`). Transit phases carry origin and
  destination timezones (`PRD.md` §10.2). All times are RFC 3339 UTC strings.
- Local display time derives from the covering trip phase, never from camera
  metadata timezone (architecture contract §3.4).
- Phases carry no membership: an asset belongs to a phase only by its trip time
  falling inside `[start_trip_time, end_trip_time]`.

### Event-order constraints

- A constraint names an earlier and a later asset, with `hard` or `soft`
  strength and a source string (`PRD.md` §9.3). The two assets must be distinct
  and must exist in the project.
- There is exactly one constraint per ordered pair: setting a constraint for a
  pair that already has one replaces it regardless of constraint id.
- Setting a constraint never writes a time.

### Operation payloads (version 1, camelCase, unknown fields denied)

| Operation | Payload fields |
| --- | --- |
| `captureSequenceDeclared` | `sequenceId`, `cameraKey`, `label`, `volumeHint`, `filenamePrefix` |
| `captureSequenceSplit` | `newSequenceId`, `cameraKey`, `label`, `volumeHint`, `filenamePrefix`, `atCounter` |
| `captureSequenceMerged` | `fromSequenceId`, `intoSequenceId` |
| `clockEpochSet` | `epochId`, `captureSequenceId`, `startCounter`, `endCounter?`, `offsetSeconds`, `label`, `confidence?`, `evidence?` |
| `clockEpochCleared` | `epochId` |
| `clipTimeExceptionSet` | `tripTime`, `reason` |
| `clipTimeExceptionCleared` | *(empty payload)* |
| `tripPhaseSet` | `phaseId`, `label`, `phaseKind`, `startTripTime`, `endTripTime`, `timezone?`, `originTimezone?`, `destinationTimezone?`, `displayMode` |
| `tripPhaseRemoved` | `phaseId` |
| `eventOrderConstraintSet` | `constraintId`, `earlierAssetId`, `laterAssetId`, `strength`, `source` |
| `eventOrderConstraintCleared` | `constraintId` |

Aggregate id equals the id field in each payload (`clipTimeException*` is the
exception: aggregate id is the asset id). All payloads are version 1 with
`deny_unknown_fields`. A set operation upserts its row; a cleared/removed
operation deletes it and is a no-op when the row does not exist. Applying the
same operation id twice materializes once.

### Revision guard

Every chronology operation participates in the `materialized_revisions`
base-revision guard. When the operation declares `base_revision` it must equal
the aggregate's current revision; otherwise the operation is refused with a
durable `operation_conflicts` row (`conflict_type` `stale_base_revision`) and
nothing is overwritten. An operation without a declared base revision passes,
matching the existing non-chronology operations. `CLOCK_EVENT_CREATED` keeps
its own legacy guard (`stale_clock_base_revision`).

### Quarantine policy (D01-T007 decision 4)

- `apply_project_operation` no longer fails on an unknown operation type.
- An unknown operation is persisted in `project_operations` with its full
  payload, is reported through `quarantined_operations`, never advances an
  aggregate revision, and never fails the containing batch. The batch cursor
  still advances, so the peer does not resend it.
- An unknown operation is not validated for payload version: a future payload
  version is exactly what a future operation type may carry. Known types still
  reject any payload version other than 1.
- `quarantined_operations(project_id)` is the set of `project_operations` rows
  whose operation type this build does not recognize, in `operation_id` order.
  `received_at` is the operation's `created_at`.
- Because the operation stays in the operation log with its complete payload,
  a build that knows the type applies it normally — either when it receives the
  operation from the peer (it materializes at apply time) or when the catalog
  is opened by that build and the operation is re-dispatched from the log.

### Re-dispatch seam

On every writable catalog open, after migration, the operation log is scanned
for persisted-but-never-applied rows of the eleven chronology operation types,
and each is re-dispatched through the normal materializer dispatch:

- Scope: only the eleven chronology operation types, with `payload_version`
  1. Legacy and pre-existing operation types are never re-dispatched.
- A row is a candidate only when it has no `operation_conflicts` row and is
  not the `last_operation_id` of its aggregate's `materialized_revisions` row.
  An operation without a declared `base_revision` is additionally a candidate
  only when its aggregate has no materialized revision of 1 or higher (a
  baseline row at revision 0 does not disqualify).
- Candidates apply in HLC order (`hlc_wall_millis`, `hlc_counter`,
  `origin_node_id`, `operation_id`), best-effort per row: a row that cannot
  apply (guard mismatch, validation bail) is left in the log and re-examined
  on the next open. The seam never fails the open and never deletes a row.
- The seam never writes `operation_conflicts` rows: a re-dispatched row whose
  declared `base_revision` no longer matches the aggregate's current revision
  is skipped, not recorded. A `stale_base_revision` conflict row therefore
  always references an operation rejected by the normal apply path, never a
  re-dispatch — so a baseline export/import cycle is unaffected by the seam.
- Read-only opens never run the seam.
- `ApplyOperationResult` gains a `Quarantined` outcome and
  `OperationBatchResult` reports `quarantined` counts. `CLOCK_EVENT_CREATED`
  and the other 18 known types are never quarantined.

### What D01-T008 through D01-T017 may not change

- The six tables, the two indexes, and migration 9's forward-migration rule.
- The operation vocabulary, payload shapes, or aggregate mapping.
- The fact types in `chronology/model.rs`, including field names and types.
- The loader contract: every `ChronologyFacts` collection sorted by primary
  key, deterministic across calls.
- The quarantine surface, the revision-guard conflict semantics, and the
  re-dispatch seam's scope, filters, ordering, and no-conflict-row
  disposition.
- The projection-column rule: `assets.best_time` and `assets.offset_seconds`
  are written only by solution projection.
- `clock_events` stays in the schema. Nothing may drop it.
