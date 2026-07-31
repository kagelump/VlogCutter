# D01-T007: Chronology domain model and operation vocabulary

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T007",
  "epic_id": "D01",
  "title": "Chronology domain model and operation vocabulary",
  "slug": "chronology-domain-model",
  "status": "ready",
  "task_class": "foundation",
  "depends_on": [
    "D01-T006"
  ],
  "principal_invariant": "Migration 9 replaces the flat clock-event model with the full persisted chronology fact model while producing the identical total review order for every existing project.",
  "central_falsification": "Capture the complete ordered asset list of several schema-8 catalogs built by independently written fixtures, run migration 9, and confirm the ordered list is unchanged; any reordering falsifies the migration.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "clock_events flat model -> persisted chronology fact model",
    "apply_project_operation dispatch -> chronology operation vocabulary",
    "sync operation exchange -> unknown operation type policy"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/desktop/ARCHITECTURE.md",
      "PRD.md"
    ],
    "implementer": [
      "PRD.md",
      "AGENTS.md",
      "docs/tasks/desktop/ARCHITECTURE.md",
      "docs/tasks/server/SCHEMA.md",
      "docs/tasks/server/SYNC_PROTOCOL.md"
    ],
    "independent_tester": [
      "PRD.md",
      "AGENTS.md",
      "docs/tasks/desktop/ARCHITECTURE.md"
    ],
    "reviewer": [
      "PRD.md",
      "AGENTS.md",
      "docs/tasks/desktop/ARCHITECTURE.md",
      "docs/tasks/server/SYNC_PROTOCOL.md"
    ],
    "integrator": [
      "docs/tasks/desktop/ARCHITECTURE.md",
      "docs/tasks/server/SCHEMA.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/desktop/tasks/D01-T007-chronology-domain-model.md",
      "docs/tasks/desktop/CHRONOLOGY_MODEL.md"
    ],
    "implementer": [
      ".gitignore",
      "AGENTS.md",
      "core/rcdvlog-core/src/chronology/mod.rs",
      "core/rcdvlog-core/src/chronology/model.rs",
      "core/rcdvlog-core/src/lib.rs",
      "core/rcdvlog-core/src/migrations.rs",
      "core/rcdvlog-core/src/operations.rs",
      "core/rcdvlog-core/tests/chronology_migration.rs",
      "core/rcdvlog-core/tests/fixtures/chronology/**"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/chronology_migration_independent.rs",
      "core/rcdvlog-core/tests/fixtures/chronology-independent/**"
    ],
    "integrator": [
      "AGENTS.md",
      "docs/task-cycles/D01-T007/**",
      "docs/tasks/desktop/EPIC.md",
      "docs/tasks/server/SCHEMA.md"
    ]
  },
  "forbidden_paths": [
    "apps/web/**",
    "apps/server/**",
    "core/rcdvlog-server/**",
    "apps/desktop/src/**",
    "apps/cli/src/**",
    ".github/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-core",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo fmt --all -- --check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T007\\].*\\| complete \\|"
    },
    {
      "path": "docs/tasks/desktop/CHRONOLOGY_MODEL.md",
      "regex": "(?s)## Frozen interface.*ChronologyFacts"
    },
    {
      "path": "docs/tasks/server/SCHEMA.md",
      "regex": "capture_sequences"
    }
  ]
}
-->

## Goal

Replace the flat per-camera clock-offset model with the persisted chronology fact
model that `PRD.md` §8–§11 describes, and publish the frozen interface that every
Priority 1 packet builds on.

Today chronology is one table — `clock_events(project_id, camera, start_filename,
offset_seconds, label)` — and one operation type, `clockEventCreated`, whose
aggregate id is a free-text camera string. There is no capture sequence, no
timezone, no trip phase, no clip exception, no ordering constraint, no
contradiction, no confidence, and no evidence. This packet is where that changes.

## Principal invariant

This task is correct when migration 9 replaces the flat clock-event model with
the full persisted chronology fact model while producing the identical total
review order for every existing project.

Order preservation is the center of the task. A richer model that silently
reshuffles a user's existing trip is a data-loss event dressed as a feature.

## Decision authority

This packet owns the decisions below and is the sole authority for them. Every
Priority 1 packet is `draft` until this packet publishes
`docs/tasks/desktop/CHRONOLOGY_MODEL.md`, at which point the contract editor
re-runs the admission gate and promotes them.

### Decision 1 — Persistence shape

Chronology facts are relational rows behind migration 9, following the existing
migration pattern in `core/rcdvlog-core/src/migrations.rs`
(`PROJECT_CATALOG_MIGRATIONS`, currently 8 entries).

| Alternative | Verdict |
| --- | --- |
| Relational tables, one per fact kind | **Chosen.** Matches every existing aggregate; queryable; migrates forward. |
| A single JSON blob column per project | Rejected. Unqueryable, unmergeable, and defeats operation-level conflict handling. |
| Extend `clock_events` with nullable columns | Rejected. Capture sequences and trip phases are not clock events; overloading the table would make every later query a special case. |
| Keep `clock_events` and add a parallel model | Rejected. Two chronology authorities is the worst outcome available. |

Acceptance criterion: every fact kind in `PRD.md` §9–§10 has a home, and no fact
is stored in more than one place.

### Decision 2 — Derived versus persisted

Persisted state is evidence only. Trip time, local display time, confidence,
evidence, uncertainty, and total order are derived by the solver (D01-T008).
`assets.best_time` and `assets.offset_seconds` remain in the schema as
projections for query performance and are written only by solution projection.

Acceptance criterion: no code path outside solution projection writes
`assets.best_time` or `assets.offset_seconds` after this task.

### Decision 3 — Operation vocabulary and aggregate mapping

New operation types dispatched from `apply_project_operation`, following the
existing constant-plus-materializer pattern.

Acceptance criterion: each new operation type has a materializer, appears in the
dispatch `match`, is idempotent on replay, and participates in the existing
`materialized_revisions` base-revision guard.

### Decision 4 — Unknown operation type policy

`apply_project_operation` currently ends its dispatch with
`bail!("unsupported operation type: {value}")`. Once the desktop emits chronology
operations, an older peer replica — the companion server at a previous version,
or a restored baseline — will receive operation types it does not know. Failing
the batch means one new operation type can wedge sync entirely.

| Alternative | Verdict |
| --- | --- |
| Retain the operation, skip materialization, report it as quarantined | **Chosen.** The operation is durable, visible, and applies after upgrade. |
| Keep failing the batch | Rejected. One unknown type wedges all sync for the project. |
| Silently drop the operation | Rejected. Silent data loss, and the operation is gone after upgrade. |
| Version-gate the whole sync session | Rejected. Blocks unrelated work and is a much larger protocol change. |

Acceptance criterion: an unknown operation type is persisted in
`project_operations`, is reported through a diagnostic surface, does not advance
the aggregate revision, does not fail the batch, and materializes correctly when
the same catalog is opened by a build that knows the type.

## Inputs and outputs

Inputs:

- `PRD.md` §8 (camera and capture sequence), §9 (chronology model), §10 (trip
  context), §11 (clock anchors), §12 (queue stability).
- The existing model: `clock_events` in `core/rcdvlog-core/src/migrations.rs`;
  `CLOCK_EVENT_CREATED`, `ClockPayload`, and `materialize_clock` in
  `core/rcdvlog-core/src/operations.rs`; `ProjectCatalog::apply_clock_event` in
  `core/rcdvlog-core/src/lib.rs`.
- `docs/tasks/server/SYNC_PROTOCOL.md` for what a peer does with an operation it
  cannot apply.

Outputs:

- Migration 9, order-preserving, forward-migrating existing `clock_events`.
- `core/rcdvlog-core/src/chronology/model.rs` with the fact types.
- `core/rcdvlog-core/src/chronology/mod.rs` exposing `ChronologyFacts` and its
  loader.
- New operation constants, payload structs, and materializers.
- The unknown-operation-type quarantine policy, implemented.
- `docs/tasks/desktop/CHRONOLOGY_MODEL.md` — the frozen interface for D01-T008
  through D01-T017.
- `docs/tasks/server/SCHEMA.md` updated so the schema of record matches reality.

## Frozen decisions

Inherited, not open for this packet to revisit:

- No chronology correction mutates source metadata (`PRD.md` §9.2).
- Every chronology mutation is a typed operation with HLC logical time, aggregate
  type and id, payload version, and base revision.
- Existing catalogs migrate forward. Delete-and-rebuild is not a migration.
- Clip exceptions ride the existing `projectAsset` aggregate so they inherit its
  revision guard rather than introducing a second guard for per-asset state.
- Filename identity is scoped to capture sequence, because two cards reuse the
  same counter (`PRD.md` §8.2).
- `AGENTS.md` §15: additive edits only to `lib.rs` and `operations.rs`. New logic
  lives in the new `chronology` module.
- Timestamps are stored as RFC 3339 strings in UTC, matching the existing
  `captured_at_raw` and `best_time` convention. Do not introduce a second time
  encoding into this schema.

## Owned paths

The contract editor owns this packet and the published model contract. The senior
implementer owns the migration, the new module, and the additive edits to
`operations.rs` and `lib.rs` — for a foundation task the migration *is* the
deliverable, so it is not delegated to the integrator. The independent tester
owns a separate test file and a separate fixture directory so their schema-8
catalogs cannot be the implementer's. The integrator owns the epic row and the
schema document of record.

The desktop, CLI, web, and server surfaces are all forbidden. This packet defines
the model; exposing it is later work. That boundary is what keeps this packet
reviewable.

## Required interface

The following is frozen by this packet and must appear in
`docs/tasks/desktop/CHRONOLOGY_MODEL.md` verbatim.

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

## Acceptance oracle

1. **Order preservation.** For each of at least five schema-8 fixture catalogs —
   one camera no clock events; one camera one clock event; two cameras one clock
   event each; two cameras three overlapping clock events on one camera; one
   camera with filenames that have no digit run — the ordered asset list is
   byte-identical before and after migration 9.
2. **Counter parsing.** `C0101.MP4` → 101, `DJI_0042.MP4` → 42, `clip.mp4` →
   NULL, `GX010234.MP4` → 10234, `IMG_1234 (1).MOV` → 1. A filename whose digit
   run exceeds `u64` does not panic; it yields NULL.
3. **Every asset is assigned.** After migration, every asset in every project has
   exactly one `asset_capture_sequence` row.
4. **Clock events carried over.** The `clock_epochs` count equals the pre-migration
   `clock_events` count, and each row's `offset_seconds` and `label` match.
5. **Idempotence.** Running the migration runner twice leaves the schema version,
   the row counts, and the order unchanged.
6. **Newer-schema refusal.** A catalog at schema 10 opened by this build reports
   `CatalogSchemaError::NewerSchema` rather than migrating downward.
7. **Operation round-trip.** For each of the eleven new operation types: applying
   the operation materializes the expected row, applying it twice materializes
   once, and applying it with a stale `base_revision` produces an
   `operation_conflicts` row rather than overwriting.
8. **Legacy replay.** A `clockEventCreated` operation applied after migration
   materializes into `clock_epochs` with the same result the migration produces
   for an equivalent legacy row.
9. **Quarantine.** An operation with type `someFutureOperation` is persisted in
   `project_operations`, appears in `quarantined_operations`, does not advance
   `materialized_revisions`, and does not cause the containing batch to fail. A
   second build that recognizes the type materializes it correctly from the
   persisted log.
10. **Loader determinism.** `chronology_facts` returns every collection in a
    deterministic order across 32 repeated calls on the same catalog.
11. **Source metadata untouched.** No code path in this change writes to a media
    file. Verify by hashing a fixture media file before and after a full
    migration and operation-application cycle.
12. **No projection writes.** Grep confirms that `assets.best_time` and
    `assets.offset_seconds` are written in exactly one place, and that place is
    not reached by migration 9.

## Independent test charter

Falsify order preservation, which is the claim the whole task rests on.

- Build your **own** schema-8 catalogs in
  `core/rcdvlog-core/tests/fixtures/chronology-independent/`. Do not import the
  implementer's fixtures. Construct them by running the schema-8 migration set
  and inserting rows directly, so they are genuinely old catalogs rather than
  new catalogs described as old.
- Include the cases the implementer is least likely to have built: two cameras
  whose filenames use the same counter range; a camera whose clock event names a
  `start_filename` that no asset has; an asset with a NULL `captured_at_raw`; two
  assets with identical `captured_at_raw` and identical filenames in different
  folders; a filename with a non-ASCII prefix and a digit run; a project with 500
  assets across three cameras.
- For each, capture the full ordered asset id list before migration, migrate,
  and assert equality. Report the first differing index.
- Attack idempotence: migrate, migrate again, and compare row counts and order.
- Attack the quarantine policy by constructing an operation with an unknown type
  and a payload version higher than any this build knows, applying it inside a
  batch with three known operations, and confirming the known three applied, the
  unknown one is retained and reported, and no revision advanced for it.
- Attack the revision guard: apply the same `clockEpochSet` twice with the same
  stale base revision and confirm two conflict rows or one conflict row and no
  overwrite — but never a silent overwrite.
- Confirm that `clock_events` still exists and still contains its original rows,
  so a rollback is possible.
- Hash a fixture media file before and after and confirm it is unchanged.

## Review charter

Risk domain: silent reordering of existing user projects, and a model that later
packets cannot build on.

Questions:

1. Can migration 9 reorder any existing project? Specifically, does it write
   `assets.best_time` or `assets.offset_seconds`, or change any value an ordering
   query reads?
2. Is every fact kind in `PRD.md` §9–§10 stored in exactly one place?
3. Does any new operation type bypass the `materialized_revisions` base-revision
   guard?
4. Is the quarantine policy actually durable — is the operation in
   `project_operations` after a restart, and does it apply after upgrade?
5. Does `chronology_facts` have any path where iteration order is unspecified? A
   `HashMap` iteration reaching output is a defect even when tests currently pass.
6. Is `clock_events` retained and is legacy operation replay still correct?
7. Is any table or column reserved for deferred work — clock anchors,
   transitions, tracks? Reserved schema for deferred features is a defect per
   the architecture contract.
8. Are `lib.rs` and `operations.rs` edited additively per `AGENTS.md` §15?
9. Does `docs/tasks/desktop/CHRONOLOGY_MODEL.md` fully specify what D01-T008
   through D01-T017 need, so those packets can be promoted without further
   design?

Severity rules: any reordering of an existing project, a bypassed revision guard,
a non-durable quarantine, unspecified iteration order reaching output, or a
mutation of source metadata is blocking. Naming, index tuning, and doc wording
are non-blocking.

Smallest acceptable remedy: for a reordering defect, remove the write that causes
it and add the fixture case that exposed it. For unspecified iteration order,
replace the collection or add an explicit sort.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Implementing the solver. That is D01-T008; this packet only defines its input.
- Proposing or detecting capture-sequence boundaries. That is D01-T009.
- Any UI, Tauri command, or CLI surface.
- Clock anchors and automatic visual clock detection, deferred by `PRD.md` §21.3.
- Server-side changes beyond updating the schema document of record.
- Changing the sync protocol version or wire format.
- Removing `clock_events` or rewriting the operation log.

## Stop conditions

Stop and escalate when:

- order preservation cannot be achieved for some existing catalog shape without a
  product decision about which order is correct;
- the quarantine policy turns out to require a sync protocol version bump, which
  is the server epic's territory and needs the user's decision;
- a fact kind in `PRD.md` §9–§10 has no clean home in this schema, indicating the
  model is wrong rather than the implementation;
- clip exceptions on the `projectAsset` aggregate conflict with the existing
  per-asset register semantics;
- the migration needs to read media files or run FFmpeg, which a migration must
  never do;
- implementing all four decisions plus the migration does not fit one focused
  context, in which case propose the split — Decision 4 and its quarantine
  implementation is the natural seam to separate.

## Commands

```sh
cargo test -p rcdvlog-core
cargo test -p rcdvlog-core --test chronology_migration
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

Then, to prove no nondeterminism reached the loader:

```sh
grep -n "HashMap\|HashSet" core/rcdvlog-core/src/chronology/*.rs
```

Expected result: every test passes, clippy is clean, and any `HashMap` or
`HashSet` found is provably not on a path that reaches ordered output — otherwise
it is replaced with a `BTreeMap`/`BTreeSet` or followed by an explicit sort.

## Handoff

Report the migration's exact behavior, the order-preservation results for every
fixture catalog with asset counts, the eleven operation types with their
aggregates and round-trip results, the quarantine policy as implemented with its
durability evidence, and the published `CHRONOLOGY_MODEL.md`. State explicitly
which Priority 1 packets are now promotable and whether any needs redesign in
light of what was learned. List any stop condition encountered. The implementer
does not approve this task.
