# D01-T020: Edit-plan validation

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T020",
  "epic_id": "D01",
  "title": "Edit-plan validation",
  "slug": "edit-plan-validation",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["D01-T019"],
  "principal_invariant": "Every invalid edit plan is rejected with a stable error code before any render or export work begins.",
  "central_falsification": "Submit a plan with a missing asset, an offline original, an out-of-bounds range, an inverted range, and a stale revision, and confirm each is refused with its own error code and that no FFmpeg process was started and no output file created.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["edit plan revision -> validation verdict"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md", "AGENTS.md"],
    "independent_tester": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md"],
    "reviewer": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md", "AGENTS.md"],
    "integrator": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T020-edit-plan-validation.md"],
    "implementer": [
      "core/rcdvlog-core/src/editplan/validate.rs",
      "core/rcdvlog-core/tests/editplan_validation.rs"
    ],
    "independent_tester": ["core/rcdvlog-core/tests/editplan_validation_independent.rs"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T020/**"]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/migrations.rs", "core/rcdvlog-core/src/chronology/**", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test editplan_validation",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T020\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Validate an edit plan completely before any expensive or irreversible work starts, so
a user discovers a bad range by being told about it rather than by watching a render
fail twenty minutes in.

## Principal invariant

This task is correct when every invalid edit plan is rejected with a stable error
code before any render or export work begins.

## Inputs and outputs

Inputs:

- `EditPlanRevision` as frozen by `docs/tasks/desktop/EDIT_PLAN_MODEL.md`.
- Asset availability state: `original_available`, `proxy_available`, and
  `duration_seconds` from the existing `Asset` type.

Outputs:

- A validation function returning every finding at once, not the first.
- Stable error codes for each finding kind, following the existing
  `CatalogSchemaError::code` and `EditPlanError::code` pattern.

## Frozen decisions

- Validation is exhaustive per call: it reports every finding, so a user fixes a plan
  in one pass rather than discovering problems one render at a time.
- Validation is pure with respect to the catalog: it reads and never writes.
- Findings are ordered deterministically by kind then by operation position, so output
  is diffable.
- The checks are exactly the `PRD.md` §23 Milestone 6 list: asset existence, media
  availability, range bounds, overlaps, unsupported operations, and revision
  freshness. No additional check is invented here.
- "Overlap" for this model means two operations referencing overlapping source ranges
  of the same asset. Timeline overlap is unrepresentable by Decision 1 of D01-T019, so
  it needs no check. Source overlap is a warning, not an error — using the same footage
  twice is legitimate.
- Staleness is a warning, not an error. `PRD.md` §21.1 step 11 requires it be visible,
  not that it block. Blocking would prevent a user from exporting a plan they know is
  fine.
- Media availability distinguishes original-offline from proxy-missing, because they
  have different consequences: preview needs a proxy, export needs a source reference.

## Owned paths

The implementer owns the validation module and its tests. Migrations, chronology, and
all application surfaces are forbidden.

## Required interface

```rust
pub enum FindingKind {
    MissingAsset,
    OriginalOffline,
    ProxyMissing,
    RangeOutOfBounds,
    RangeInverted,
    RangeZeroLength,
    SourceRangeOverlap,
    StaleScriptRevision,
    StaleRevision,
}

pub enum Severity { Error, Warning }

pub struct Finding {
    pub kind: FindingKind,
    pub severity: Severity,
    pub code: &'static str,
    pub position: Option<u32>,
    pub asset_id: Option<String>,
    pub message: String,
}

pub struct ValidationReport {
    pub findings: Vec<Finding>,
}

impl ValidationReport {
    /// True when no finding has severity Error.
    pub fn renderable(&self) -> bool;
    pub fn errors(&self) -> impl Iterator<Item = &Finding>;
    pub fn warnings(&self) -> impl Iterator<Item = &Finding>;
}

impl ProjectCatalog {
    pub fn validate_edit_plan(&self, revision_id: &str) -> Result<ValidationReport>;
}
```

Error codes, stable and referenced by the UI and CLI:

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

`OriginalOffline` is an error because export writes source references a finishing
editor must resolve; `ProxyMissing` is a warning because a proxy can be generated.

## Acceptance oracle

1. A plan referencing a deleted asset yields `MissingAsset` at the right position.
2. A plan whose asset original is offline yields `OriginalOffline`; `renderable()` is
   false.
3. A plan whose asset has no proxy yields `ProxyMissing` as a warning;
   `renderable()` stays true.
4. `source_out_seconds` exceeding `duration_seconds` yields `RangeOutOfBounds`.
5. `source_out_seconds` below `source_in_seconds` yields `RangeInverted`.
6. Equal in and out yields `RangeZeroLength`.
7. A negative `source_in_seconds` yields `RangeOutOfBounds`.
8. Two operations using overlapping ranges of one asset yield
   `SourceRangeOverlap` as a warning only.
9. A plan whose recorded script revision is not the active one yields
   `StaleScriptRevision` as a warning.
10. A plan with five distinct problems reports all five in one report, ordered
    deterministically.
11. A valid plan reports zero findings and `renderable()` is true.
12. An empty plan reports zero findings and `renderable()` is true.
13. Validation writes nothing: the catalog file hash is unchanged after validating a
    plan with every finding kind present.
14. Validation starts no child process: no FFmpeg or ffprobe invocation occurs.

## Independent test charter

Falsify the claim that nothing expensive happens before validation passes.

- Construct a plan where the very first operation is valid and the last is invalid,
  and confirm validation still reports the last one — a short-circuit at the first
  error would hide it.
- Attack floating-point boundaries: `source_out_seconds` exactly equal to
  `duration_seconds`; a range one microsecond beyond it; a range with
  `f64::NAN`, `f64::INFINITY`, and `-0.0` in each field. None may panic; each must
  produce a definite verdict.
- Attack availability by making a file unreadable rather than absent, and by pointing
  an asset at a directory.
- Confirm no process is spawned by validating under a `PATH` that contains no
  `ffmpeg` or `ffprobe`, and confirming validation still completes normally.
- Confirm read-only behavior by hashing the catalog file before and after.
- Assert determinism by validating the same plan 32 times and comparing the full
  report including finding order.
- Confirm every code string in the table above appears in the implementation and that
  no other code strings were introduced.

## Review charter

Risk domain: work started on an invalid plan, and severity choices that either block
legitimate work or permit broken output.

Questions:

1. Does validation short-circuit anywhere, hiding later findings?
2. Is any check missing from the `PRD.md` §23 Milestone 6 list, or any extra check
   invented?
3. Are the severity assignments right — specifically, does a warning ever permit an
   export that cannot be opened?
4. Does validation write to the catalog or spawn a process?
5. Are error codes stable strings matching the table, and used nowhere else with a
   different meaning?
6. Do NaN and infinity produce a definite verdict rather than a panic or a silent
   pass?
7. Is finding order deterministic?

Severity rules: a short-circuit, a missing check, a write, a spawned process, or a
panic on a pathological float is blocking. Message wording is non-blocking.

Smallest acceptable remedy: collect all findings before returning, and add the case
that exposed the short-circuit.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Rendering or exporting, which are D01-T021 and D01-T022.
- Repairing an invalid plan or suggesting fixes.
- Any UI or CLI surface.
- Validating chronology, which D01-T013 owns.
- Adding checks beyond the frozen list.

## Stop conditions

Stop and escalate when:

- a check requires probing media with ffprobe, which would make validation
  non-instant and process-dependent — escalate rather than accepting the cost;
- the severity of a finding is genuinely ambiguous for a real case;
- `duration_seconds` proves unreliable for bounds checking on real media, meaning
  bounds cannot be validated without probing;
- a finding cannot be attributed to a specific operation position;
- the boundary between what D01-T019 stores and what this packet validates turns out
  ambiguous.

## Commands

```sh
cargo test -p rcdvlog-core --test editplan_validation
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected result: all tests pass and clippy is clean.

## Handoff

Report every finding kind with its severity and code, the exhaustiveness evidence
showing all findings returned in one pass, the float-boundary results, the
no-write and no-process evidence, and the determinism result. List any stop condition
encountered. The implementer does not approve this task.

## Promotion note

`draft` until D01-T019 publishes `docs/tasks/desktop/EDIT_PLAN_MODEL.md`. The contract
editor reconciles the finding list and the severity assignments with the frozen model
before promoting, and confirms the store-versus-validate boundary is unambiguous.
