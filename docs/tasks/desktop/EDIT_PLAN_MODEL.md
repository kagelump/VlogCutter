# Edit-plan model — frozen contract

Published by D01-T019
(`docs/tasks/desktop/tasks/D01-T019-edit-plan-model.md`), 2026-08-03. This
document freezes the edit-plan persistence model, the operation vocabulary, and
the revision semantics for D01-T020 through D01-T023.

## Status

- Project catalog schema: 12 (migration 12, `edit_plan`).
- `docs/tasks/server/SCHEMA.md` is updated to match.

**Migration numbering note.** D01-T019's packet says "migration 10". Migrations
10 and 11 were taken by D01-T027 (diagnostics) and D01-T028 (camera filename
patterns), which were inserted into the queue ahead of the edit plan. The edit
plan is migration **12**. Nothing else about the packet's contract changed.

## Scope, stated as an exclusion

The editing scope is exhaustively:

- one video track,
- linked source audio,
- hard cuts,
- source in/out ranges,
- ordered operations,
- low-resolution preview,
- deterministic OTIO export.

There is **no** `track` column, **no** `transition` table, and **no** `effect`
column — not even as reserved schema. That absence is the deliverable, not an
omission for a later packet to fill in. A later feature is a later migration
with a later contract.

## Required interface

### Tables, migration 12

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

`UNIQUE(revision_id, position)` makes two clips at one position
unrepresentable, and the partial unique index makes two active revisions
unrepresentable. Both invariants are enforced by the database rather than by
application discipline, because discipline is what fails.

### Operation vocabulary

```rust
pub const EDIT_PLAN_REVISION_CREATED: &str = "editPlanRevisionCreated";
pub const EDIT_PLAN_REVISION_ACTIVATED: &str = "editPlanRevisionActivated";
pub const EDIT_PLAN_EXPORT_RECORDED: &str = "editPlanExportRecorded";
```

Aggregate type `editPlan`, aggregate id the **project id**, because activation
is a project-level fact and the active revision is what actually conflicts. All
three are idempotent on replay (`INSERT OR IGNORE` throughout) and honour the
base-revision guard.

### Rust types

See `core/rcdvlog-core/src/editplan/model.rs`: `EditPlanOperation`,
`EditPlanRevision`, `EditPlanStaleness`, `EditPlanExport`, and `EditPlanError`
with codes `edit_plan_stale_revision` and `edit_plan_unknown_revision`.

### Catalog interface

```rust
impl ProjectCatalog {
    fn create_edit_plan_revision(&self, project_id: &str, operations: &[EditPlanOperation],
        script_revision_id: Option<&str>, expected_active_revision_id: Option<&str>)
        -> Result<EditPlanRevision>;
    fn activate_edit_plan_revision(&self, revision_id: &str,
        expected_active_revision_id: Option<&str>) -> Result<EditPlanRevision>;
    fn active_edit_plan(&self, project_id: &str) -> Result<Option<EditPlanRevision>>;
    fn edit_plan_revision(&self, revision_id: &str) -> Result<EditPlanRevision>;
    fn edit_plan_revisions(&self, project_id: &str) -> Result<Vec<EditPlanRevision>>;
    fn edit_plan_staleness(&self, revision_id: &str) -> Result<EditPlanStaleness>;
    fn record_edit_plan_export(&self, export: &EditPlanExport) -> Result<()>;
    fn edit_plan_exports(&self, project_id: &str) -> Result<Vec<EditPlanExport>>;
}
```

## Revision semantics

**A revision can never silently replace a newer one.** Both create and activate
take the revision the caller *believed* was active and refuse with
`EditPlanError::StaleRevision` if that is no longer true, naming the revision
that superseded it.

`expected_active_revision_id` is `Option` only to express "no plan exists yet".
Once a plan exists, passing `None` is refused rather than treated as a force.

**A refused revision is not lost.** It remains in `edit_plan_revisions` and can
be re-derived from the winner. Nothing in this model deletes a revision.

**Staleness is derived, never stored.** `edit_plan_staleness` compares the
recorded script revision against the active one at read time; storing it would
let it drift from the thing it describes.

**An empty plan is a legitimate state.** `EditPlanRevision::is_empty` reports
it; no caller may treat it as an error.

## Validation (D01-T020)

Errors block render and export; warnings inform. The codes are stable and
referenced by the desktop and CLI:

| Kind | Severity | Code |
| --- | --- | --- |
| `MissingAsset` | Error | `edit_plan_missing_asset` |
| `OriginalOffline` | Error | `edit_plan_original_offline` |
| `ProxyMissing` | Warning | `edit_plan_proxy_missing` |
| `RangeOutOfBounds` | Error | `edit_plan_range_out_of_bounds` |
| `RangeInverted` | Error | `edit_plan_range_inverted` |
| `RangeZeroLength` | Error | `edit_plan_range_zero_length` |
| `SourceRangeOverlap` | Warning | `edit_plan_source_range_overlap` |
| `StaleScriptRevision` | Warning | `edit_plan_stale_script_revision` |
| `StaleRevision` | Warning | `edit_plan_stale_revision_warning` |

An offline **original** blocks, because the export conforms against camera
masters. A missing **proxy** only warns, because it just makes the preview
slower. Findings are ordered by position then code, so a report reads down the
timeline.

## Preview (D01-T021)

Renders from review proxies, falling back to the original when no proxy exists.
Validation runs first: a plan that is not `renderable()` is refused before
FFmpeg is invoked, and that refusal is a refusal, not a render failure.

Output lives under `cache/previews/<project_id>/<revision_id>/preview.mp4`, so
one revision's preview is never confused with another's. Per-clip failures are
surfaced with the clip that caused them and never abort the batch silently.
Cancellation is polled between clips and leaves nothing that could be mistaken
for a finished preview. The final file is renamed into place only once complete.

Hard cuts only: segments are cut individually and joined with the concat
demuxer, so there is no filter graph that could introduce a transition
incidentally. The preview is not an export and has no determinism requirement.

## OTIO export (D01-T022)

A pure-Rust writer. No Python, no `otiotool`, no shelling out to the reference
implementation — `PRD.md` §21.3 forbids a permanent Python dependency. The
accepted cost is that this repository owns schema fidelity, which is exactly why
D01-T024's real DaVinci Resolve import gate is required rather than optional.

Determinism is structural:

- JSON keys are written in a fixed order; no map type reaches serialization.
- A clip's identifier is `<revision_id>:<position padded to 5>`, a function of
  stable input only, so re-export produces the same clips rather than new ones.
- Time is rational — a frame count and a rate — never a bare float. Seconds
  convert to frames by rounding half away from zero, then clamping to zero.
- Nothing environmental reaches the output: no timestamp, hostname, username,
  temporary path, process id, or random value.

Source references are the **originals**, never the proxies: a finishing editor
conforms against camera masters.

An existing output is never silently replaced. `export_otio` refuses unless the
caller supplies `expected_existing_digest` matching what is actually on disk,
and the refusal names the digest found (`otio_output_exists`).

## What D01-T020 through D01-T023 may not change

- The three tables, the partial unique index, and migration 12's shape.
- The operation vocabulary and the `editPlan`/project-id aggregate mapping.
- The record types, including field names and types.
- The stale-revision refusal semantics, including that `None` is not a force.
- The rule that no revision is ever deleted.
- The absence of `track`, `transition`, `effect`, `filter`, `opacity`,
  `audio_level`, and `fade` from the schema and the writer.
- The OTIO determinism rules and the originals-not-proxies rule.


### Revision-safe CLI foundation (schema 49)

Migration 49 preserves all existing rows and scopes the operation primary key to
`(revision_id, operation_id)`. This allows stable cut IDs across immutable drafts;
`UNIQUE(revision_id, position)` and the single-active-revision index remain.
Creation and activation check expected active state in the same immediate
transaction as the operation commit, including an assertion of no active plan.
Activation additionally refuses blocking validation findings without deleting the
draft. `create_edit_plan_from_base` permits explicit same-project draft ancestry
independent of the active-plan guard. See [CLI contract](../../agent-cli.md#immutable-edit-plans)
for ordered JSON input and cut-identity diffs. Ranges retain source seconds without
persistence snapping; preview seeks no longer round them to milliseconds.
