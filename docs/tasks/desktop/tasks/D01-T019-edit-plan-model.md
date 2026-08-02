# D01-T019: Edit-plan model and revision semantics

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T019",
  "epic_id": "D01",
  "title": "Edit-plan model and revision semantics",
  "slug": "edit-plan-model",
  "status": "complete",
  "task_class": "foundation",
  "depends_on": ["D01-T014"],
  "principal_invariant": "An edit-plan revision can never silently replace a newer one, and the persisted model can express nothing beyond one video track of hard cuts with linked source audio and source in/out ranges.",
  "central_falsification": "Create two revisions from the same base, commit the second, then commit the first and confirm it is refused with the newer revision identified; separately, attempt to express a second track, a transition, and an effect and confirm the model has no field that accepts any of them.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": [
    "project catalog schema -> edit plan persistence",
    "script revision -> edit plan staleness"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md", "PRD.md"],
    "implementer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md", "docs/tasks/desktop/CHRONOLOGY_MODEL.md"],
    "independent_tester": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "reviewer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/desktop/ARCHITECTURE.md", "docs/tasks/server/SCHEMA.md"]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/desktop/tasks/D01-T019-edit-plan-model.md",
      "docs/tasks/desktop/EDIT_PLAN_MODEL.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/editplan/mod.rs",
      "core/rcdvlog-core/src/editplan/model.rs",
      "core/rcdvlog-core/src/migrations.rs",
      "core/rcdvlog-core/src/operations.rs",
      "core/rcdvlog-core/src/lib.rs",
      "core/rcdvlog-core/tests/editplan_model.rs"
    ],
    "independent_tester": ["core/rcdvlog-core/tests/editplan_model_independent.rs"],
    "integrator": [
      "docs/tasks/desktop/EPIC.md",
      "docs/tasks/server/SCHEMA.md",
      "docs/task-cycles/D01-T019/**"
    ]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/chronology/**", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test editplan_model",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "cargo fmt --all -- --check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T019\\].*\\| complete \\|"
    },
    {
      "path": "docs/tasks/desktop/EDIT_PLAN_MODEL.md",
      "regex": "(?s)## Frozen interface.*EditPlanOperation"
    }
  ]
}
-->

## Goal

Define and persist the edit plan, at exactly the accepted scope and no wider, with
revision semantics that make silent replacement of newer work impossible.

## Principal invariant

This task is correct when an edit-plan revision can never silently replace a newer
one, and the persisted model can express nothing beyond one video track of hard cuts
with linked source audio and source in/out ranges.

Both halves are the point. The second half is a scope defense: `PRD.md` §20 names
scope expansion into full editing as a primary project risk, and the only durable
defense is a data model that cannot represent a transition.

## Decision authority

This packet owns the decisions below. D01-T020 through D01-T024 are `draft` until it
publishes `docs/tasks/desktop/EDIT_PLAN_MODEL.md`.

### Decision 1 — Timeline representation

Timeline order is the index of the operation in an ordered list. There is no
timeline-time field.

| Alternative | Verdict |
| --- | --- |
| Ordered list, position is index | **Chosen.** Hard cuts with no gaps make order sufficient; nothing can fall out of sync with it. |
| Explicit timeline in/out per clip | Rejected. Two sources of truth for the same fact, and every edit must maintain both. |
| Timeline start time plus duration | Rejected. Permits gaps and overlaps the accepted scope forbids, and invites transitions later. |

Acceptance criterion: the model cannot express a gap, an overlap, or two clips at the
same timeline position.

### Decision 2 — Track and audio representation

There is one video track with linked source audio, represented by the absence of any
track field. Audio is not a separate entity; it is the clip's own audio, carried
implicitly.

| Alternative | Verdict |
| --- | --- |
| No track field at all | **Chosen.** Enforces single-track by construction. |
| `track: u8` defaulting to 0 | Rejected. Reserved schema for deferred work; the first multi-track request becomes a one-line change with no design review. |
| Separate audio operation list | Rejected. Linked source audio means the audio is the clip's, and a second list would allow them to diverge. |

Acceptance criterion: no field, column, or enum variant in the model refers to a
track, a transition, an effect, a filter, colour, or an audio level.

### Decision 3 — Revision semantics

An edit plan is a sequence of immutable revisions with one active revision. A
mutation names the revision it expects to replace.

| Alternative | Verdict |
| --- | --- |
| Immutable revisions, expected-revision guard on mutation | **Chosen.** Matches the existing note-revision and script-revision discipline. |
| Mutable single plan with last-write-wins | Rejected. `PRD.md` §21.1 step 11 requires that two edits from the same base cannot silently replace one another. |
| Full operation-level CRDT merge of plan contents | Rejected. Far larger than the accepted scope, and an edit plan is a deliberate authored artifact where a merge would produce a timeline nobody chose. |

Acceptance criterion: committing a revision whose expected base is no longer active
is refused, and the refusal names the revision that superseded it.

### Decision 4 — Script staleness

A plan records the script revision it derived from. When the active script revision
moves on, the plan is stale.

Acceptance criterion: staleness is derived by comparing recorded and active script
revisions, never stored as a mutable flag that can drift.

## Inputs and outputs

Inputs:

- `PRD.md` §21.2 (launch-critical capabilities), §21.4 (explicitly deferred), §23
  Milestone 6, §24.1 scenario E (concurrent artifact revisions).
- The existing revision patterns: `note_revisions` and `materialized_revisions` in
  `core/rcdvlog-core/src/migrations.rs`, and the operation model in
  `core/rcdvlog-core/src/operations.rs`.
- Solved chronology order from D01-T014, which is the default order a plan is built
  from.

Outputs:

- Migration 10 with the edit-plan tables.
- `core/rcdvlog-core/src/editplan/model.rs` with the types.
- New operation constants, payloads, and materializers.
- `docs/tasks/desktop/EDIT_PLAN_MODEL.md` — the frozen contract for D01-T020
  through D01-T024.
- `docs/tasks/server/SCHEMA.md` updated.

## Frozen decisions

Inherited and not open here:

- Every mutation is a typed operation with HLC time, aggregate type and id, payload
  version, and base revision.
- Existing catalogs migrate forward.
- Source media is immutable; a plan references it and never modifies it.
- `AGENTS.md` §15: additive edits only to `lib.rs` and `operations.rs`.
- Deferred and not to be represented: fine-cut editing, transitions, effects, colour,
  audio mixing, general-purpose timeline editing (`PRD.md` §21.4).
- Times are RFC 3339 UTC strings; durations and source ranges are seconds as `f64`,
  matching the existing `duration_seconds`, `source_in`, and `source_out` columns.

## Owned paths

The senior implementer owns migration 10, the new module, and the additive edits to
`operations.rs` and `lib.rs`. The contract editor owns the published model contract.
The chronology modules are forbidden: the plan consumes chronology order and must not
change it.

## Required interface

Frozen by this packet and reproduced verbatim in
`docs/tasks/desktop/EDIT_PLAN_MODEL.md`.

### Tables, migration 10

```sql
CREATE TABLE IF NOT EXISTS edit_plan_revisions (
    revision_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    script_revision_id TEXT,
    parent_revision_id TEXT,
    active INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS edit_plan_operations (
    operation_id TEXT PRIMARY KEY,
    revision_id TEXT NOT NULL REFERENCES edit_plan_revisions(revision_id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    source_in_seconds REAL NOT NULL,
    source_out_seconds REAL NOT NULL,
    rationale TEXT NOT NULL DEFAULT '',
    UNIQUE(revision_id, position)
);

CREATE TABLE IF NOT EXISTS edit_plan_exports (
    export_id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    revision_id TEXT NOT NULL REFERENCES edit_plan_revisions(revision_id) ON DELETE CASCADE,
    output_path TEXT NOT NULL,
    output_digest TEXT NOT NULL,
    exported_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS edit_plan_one_active
ON edit_plan_revisions(project_id) WHERE active = 1;
```

`UNIQUE(revision_id, position)` makes two clips at one position unrepresentable, and
the partial unique index makes two active revisions unrepresentable. Both invariants
are enforced by the database rather than by application discipline.

There is no `track` column, no `transition` table, and no `effect` column. That
absence is the deliverable of Decision 2.

### Operation vocabulary

```rust
pub const EDIT_PLAN_REVISION_CREATED: &str = "editPlanRevisionCreated";
pub const EDIT_PLAN_REVISION_ACTIVATED: &str = "editPlanRevisionActivated";
pub const EDIT_PLAN_EXPORT_RECORDED: &str = "editPlanExportRecorded";
```

Aggregate type `editPlan`, aggregate id the project id, because activation is a
project-level fact and the active revision is what conflicts.

### Rust types

```rust
pub struct EditPlanOperation {
    pub operation_id: String,
    pub position: u32,
    pub asset_id: String,
    pub source_in_seconds: f64,
    pub source_out_seconds: f64,
    pub rationale: String,
}

pub struct EditPlanRevision {
    pub revision_id: String,
    pub project_id: String,
    pub script_revision_id: Option<String>,
    pub parent_revision_id: Option<String>,
    pub active: bool,
    pub operations: Vec<EditPlanOperation>,
    pub created_at: String,
}

pub struct EditPlanStaleness {
    pub stale: bool,
    pub recorded_script_revision_id: Option<String>,
    pub active_script_revision_id: Option<String>,
}

pub struct EditPlanExport {
    pub export_id: String,
    pub revision_id: String,
    pub output_path: String,
    pub output_digest: String,
    pub exported_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EditPlanError {
    #[error("edit plan revision {expected} is no longer active; {actual} superseded it")]
    StaleRevision { expected: String, actual: String },
    #[error("edit plan revision {revision_id} not found")]
    UnknownRevision { revision_id: String },
}

impl EditPlanError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StaleRevision { .. } => "edit_plan_stale_revision",
            Self::UnknownRevision { .. } => "edit_plan_unknown_revision",
        }
    }
}

impl ProjectCatalog {
    pub fn create_edit_plan_revision(
        &self,
        project_id: &str,
        operations: &[EditPlanOperation],
        script_revision_id: Option<&str>,
        expected_active_revision_id: Option<&str>,
    ) -> Result<EditPlanRevision>;

    pub fn activate_edit_plan_revision(
        &self,
        revision_id: &str,
        expected_active_revision_id: Option<&str>,
    ) -> Result<EditPlanRevision>;

    pub fn active_edit_plan(&self, project_id: &str) -> Result<Option<EditPlanRevision>>;
    pub fn edit_plan_revision(&self, revision_id: &str) -> Result<EditPlanRevision>;
    pub fn edit_plan_staleness(&self, revision_id: &str) -> Result<EditPlanStaleness>;
    pub fn record_edit_plan_export(&self, export: &EditPlanExport) -> Result<()>;
    pub fn edit_plan_exports(&self, project_id: &str) -> Result<Vec<EditPlanExport>>;
}
```

`expected_active_revision_id` is `Option` only to express "no plan exists yet". Once
a plan exists, passing `None` is refused rather than treated as a force.

## Acceptance oracle

1. Creating a revision from operations persists them with positions 0..n-1 in the
   given order, and reading them back returns the same order.
2. Two clients create revisions B and C from the same active revision A. C activates.
   B's activation is refused with `EditPlanError::StaleRevision` naming C. B is not
   lost — it remains a revision and can be re-derived from C.
3. Passing `expected_active_revision_id: None` when a plan already exists is refused.
4. The partial unique index prevents two active revisions for one project, verified by
   attempting the insert directly and observing the constraint error.
5. `UNIQUE(revision_id, position)` prevents two operations at one position, verified
   the same way.
6. `edit_plan_staleness` reports stale when the active script revision differs from
   the recorded one, and not stale when they match. Staleness is never stored.
7. A revision with zero operations is valid — an empty plan is a legitimate state —
   and reports its own emptiness rather than erroring.
8. Migration 10 is idempotent, leaves every existing table untouched, and does not
   change any project's asset order.
9. A catalog at schema 9 opened by this build migrates to 10; a catalog at 11 reports
   `NewerSchema`.
10. Each of the three operation types is idempotent on replay and honours the base
    revision guard.
11. `record_edit_plan_export` preserves prior exports; export history is append-only.
12. Grep of `core/rcdvlog-core/src/editplan/` and migration 10 finds no occurrence of
    `track`, `transition`, `effect`, `filter`, `opacity`, `audio_level`, or `fade`.

## Independent test charter

Falsify the no-silent-replacement claim and the scope-containment claim.

- Build your own scenario for `PRD.md` §24.1 scenario E: three concurrent revisions
  from one base, activated in every order, and assert that in every ordering exactly
  one activation succeeds per base and each refusal names the actual active revision.
- Attack the guard directly: activate with a `expected_active_revision_id` naming a
  revision from a different project; naming a revision that never existed; naming the
  revision being activated.
- Attack the database invariants by inserting rows directly, bypassing the Rust
  layer, and confirm the constraints hold. An invariant enforced only in application
  code is not enforced.
- Attack scope containment: attempt to persist two operations at the same position;
  attempt an operation with `source_out_seconds` below `source_in_seconds` (this
  packet stores it; D01-T020 validates it — confirm which layer is responsible and
  that the boundary is documented); grep for every forbidden field name.
- Attack migration 10 on a catalog carrying real chronology data from migration 9,
  and confirm the asset order is unchanged.
- Confirm staleness is derived by mutating the active script revision directly and
  observing staleness change with no edit-plan write.
- Confirm export history is append-only by recording three exports and asserting all
  three are present in insertion order.

## Review charter

Risk domain: silent replacement of authored work, and reserved schema that quietly
licenses scope expansion.

Questions:

1. Can any path replace an active revision without an expected-revision check?
2. Are the single-active and unique-position invariants enforced by the database or
   only by Rust?
3. Is staleness derived, or stored where it can drift?
4. Does any table, column, enum variant, or type name refer to a track, transition,
   effect, colour, or audio level?
5. Can the model express a gap or an overlap on the timeline?
6. Does migration 10 change any existing behavior or ordering?
7. Is a refused activation genuinely non-destructive to the refused revision?

Severity rules: a bypassable revision guard, an invariant enforced only in
application code, stored staleness, or any reserved field for deferred editing
features is blocking. Naming and index tuning are non-blocking.

Smallest acceptable remedy: add the database constraint, or delete the reserved field
and the code that referenced it.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Validation of plan contents, which is D01-T020. This packet persists; it does not
  judge.
- Preview rendering, which is D01-T021.
- OTIO export, which is D01-T022. This packet records exports but writes no file.
- Any UI or CLI surface.
- Script revision management itself, which already exists.
- Merging plan contents across revisions.

## Stop conditions

Stop and escalate when:

- the accepted scope cannot express something the MVP journey in `PRD.md` §21.1
  step 12 actually requires;
- the single-active-revision invariant cannot be enforced by the schema on the
  SQLite version in use;
- the revision guard conflicts with the existing `materialized_revisions` mechanism;
- migration 10 cannot avoid touching chronology tables;
- the boundary between what this packet stores and what D01-T020 validates is
  ambiguous for a real case — resolve it in the contract before implementing.

## Commands

```sh
cargo test -p rcdvlog-core --test editplan_model
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
grep -rniE "track|transition|effect|filter|opacity|audio_level|fade" core/rcdvlog-core/src/editplan/
```

Expected result: all tests pass, clippy is clean, and the grep returns nothing —
any hit must be justified as unrelated vocabulary or removed.

## Handoff

Report the schema as implemented with its enforced invariants, the three operation
types and their replay results, the concurrent-revision scenario results for every
activation order, the staleness derivation evidence, the migration idempotence and
order-preservation results, and the forbidden-vocabulary grep output. State which
Priority 2 packets are now promotable. List any stop condition encountered. The
implementer does not approve this task.
