# D01-T011: Clip time exceptions

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T011",
  "epic_id": "D01",
  "title": "Clip time exceptions",
  "slug": "clip-time-exceptions",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["D01-T010"],
  "principal_invariant": "A clip time exception overrides sequence-derived time for exactly one asset and is always surfaced as explicit evidence on that asset.",
  "central_falsification": "Set an exception on one clip inside an epoch range, then confirm that clip alone uses the exception time, that its neighbours are untouched, and that its evidence names the exception.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["clip exception -> derived asset time"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T011-clip-time-exceptions.md"],
    "implementer": [
      "core/rcdvlog-core/src/chronology/exceptions.rs",
      "core/rcdvlog-core/tests/chronology_exceptions.rs"
    ],
    "independent_tester": ["core/rcdvlog-core/tests/chronology_exceptions_independent.rs"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T011/**"]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/migrations.rs", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test chronology_exceptions",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T011\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Let the user pin one clip's trip time directly, for the rare clip that genuinely
breaks its camera's filename chain — a clip copied from another device, a clip
whose camera clock was reset mid-card, a clip re-encoded by another tool.

`PRD.md` §9.4 requires this to be explicit and visible, because it weakens the
monotonic filename assumption everything else relies on.

## Principal invariant

This task is correct when a clip time exception overrides sequence-derived time
for exactly one asset and is always surfaced as explicit evidence on that asset.

## Inputs and outputs

Inputs:

- The frozen `ClipTimeException` type and the `clipTimeExceptionSet` /
  `clipTimeExceptionCleared` operations, which ride the `projectAsset` aggregate.
- Epoch-derived times from D01-T010.

Outputs:

- Exception resolution inside time derivation: an exception wins over every epoch.
- `ProjectCatalog` methods to set and clear an exception.
- An `Evidence` entry of kind `clip_time_exception` on every affected asset,
  carrying the user's stated reason.

## Frozen decisions

- An exception sets an absolute trip time, not an offset. An offset would be
  ambiguous when the underlying raw timestamp is missing, which is a common reason
  to need an exception in the first place.
- An exception beats every epoch. It is the most specific fact available about one
  asset.
- One exception per asset. Setting a second replaces the first, through the
  existing `projectAsset` revision guard rather than a new mechanism.
- An exception is always visible: it appears in the asset's evidence list and
  lowers no confidence silently. Confidence reflects that a human asserted the
  time.
- An exception never modifies `captured_at_raw` and never modifies source
  metadata. The raw timestamp remains visible alongside the override so the user
  can see what they overrode.
- Clearing an exception restores whatever epochs imply, not a snapshot.

## Owned paths

The implementer owns the exceptions module and its tests. Sequence and epoch
modules are not owned here; if this task needs to change them, that is a stop
condition indicating the seam is wrong.

## Required interface

```rust
impl ProjectCatalog {
    pub fn set_clip_time_exception(
        &self,
        asset_id: &str,
        trip_time: &str,
        reason: &str,
        expected_revision: u64,
    ) -> Result<ClipTimeException>;

    pub fn clear_clip_time_exception(
        &self,
        asset_id: &str,
        expected_revision: u64,
    ) -> Result<()>;
}
```

`expected_revision` is required, not optional, because an exception can replace
another user's or another node's exception and silent replacement is exactly what
the operation model exists to prevent.

## Acceptance oracle

1. An exception on one clip inside an epoch range: that clip uses the exception
   time; every other asset in the range keeps its epoch-derived time.
2. The affected asset's evidence contains an entry of kind
   `clip_time_exception` carrying the reason string.
3. The affected asset's `raw_time` still reports the original camera timestamp.
4. An exception on an asset with a null `captured_at_raw` gives that asset a
   concrete trip time and a definite order position.
5. Clearing the exception returns the asset to its epoch-derived time, and to a
   null trip time when it had no raw timestamp.
6. Setting an exception with a stale `expected_revision` produces an
   `operation_conflicts` row and does not overwrite.
7. Setting an exception twice with the correct revision each time leaves exactly
   one exception row.
8. An exception whose `trip_time` is not valid RFC 3339 is rejected with a stable
   error code before any write.
9. An exception placing a clip before its sequence predecessor does not panic and
   does not corrupt order; it produces a soft contradiction candidate for
   D01-T013 to report.
10. `captured_at_raw` is unchanged for every asset after every operation.

## Independent test charter

Falsify the "exactly one asset" claim and the visibility claim.

- Build your own corpus where three assets share the same counter across three
  sequences, and set an exception on one. Assert the other two are untouched — a
  bug here is easy to write and easy to miss.
- Set exceptions on the first asset, the last asset, and an asset with a null
  counter.
- Set an exception whose time is far outside the trip — the year 1970 and the year
  2200 — and confirm ordering stays total and nothing panics.
- Set an exception equal to a neighbour's derived time to the millisecond and
  confirm the total order still has no ties.
- Attack the revision guard by applying two exception operations built from the
  same base revision and confirming one conflicts.
- Confirm the reason string survives a non-ASCII value and a 2,000-character
  value without truncation or panic.
- Confirm the evidence entry is present on the asset by reading it back through
  the public surface, not by inspecting the table directly.

## Review charter

Risk domain: an override that silently affects neighbours, or one that becomes
invisible so a future reader cannot tell why a clip sits where it does.

Questions:

1. Can an exception affect any asset other than its own?
2. Is the exception always present in the asset's evidence?
3. Is `raw_time` preserved and still reported?
4. Is `expected_revision` genuinely enforced?
5. Does an exception silently create an ordering contradiction without recording a
   candidate for reporting?
6. Is invalid input rejected before any write?

Severity rules: cross-asset effect, a missing evidence entry, a bypassed revision
guard, or a write to `captured_at_raw` is blocking. Reason-length limits and error
wording are non-blocking.

Smallest acceptable remedy: narrow the predicate or add the evidence entry, plus
the test case that exposed the gap.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Contradiction detection and reporting, which is D01-T013.
- Clock anchors and automatic clock detection, deferred by `PRD.md` §21.3.
- Any UI for setting an exception.
- Bulk exception editing.
- Changing epoch or sequence behavior.

## Stop conditions

Stop and escalate when:

- riding the `projectAsset` aggregate conflicts with the existing per-asset
  register semantics for notes, labels, removal, or rotation;
- an exception needs to express a relative offset rather than an absolute time to
  serve a real case;
- implementing this requires changing `epochs.rs` or `sequences.rs`;
- an exception that contradicts a hard constraint needs a blocking decision that
  D01-T013 has not yet defined;
- confidence semantics for a human-asserted time are not specified by the model.

## Commands

```sh
cargo test -p rcdvlog-core --test chronology_exceptions
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected result: all tests pass and clippy is clean.

## Handoff

Report the override precedence as implemented, the single-asset containment
results, the evidence-visibility verification through the public surface, the
revision-guard conflict result, and confirmation that `captured_at_raw` is never
written. List any stop condition encountered. The implementer does not approve this
task.

## Promotion note

`draft` until D01-T007 publishes `docs/tasks/desktop/CHRONOLOGY_MODEL.md`. The
contract editor confirms the `projectAsset` aggregate decision survived
implementation before promoting.
