# D01-T010: Clock epochs over capture-sequence ranges

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T010",
  "epic_id": "D01",
  "title": "Clock epochs over capture-sequence ranges",
  "slug": "clock-epoch-ranges",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["D01-T009"],
  "principal_invariant": "Applying one clock epoch changes derived time for every asset inside its declared counter range and for no asset outside it.",
  "central_falsification": "Record derived times for a 200-clip sequence, apply an epoch bounded to counters 150 through 160, and assert exactly those eleven assets moved.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["clock epoch range -> derived asset time"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T010-clock-epoch-ranges.md"],
    "implementer": [
      "core/rcdvlog-core/src/chronology/epochs.rs",
      "core/rcdvlog-core/tests/chronology_epochs.rs"
    ],
    "independent_tester": ["core/rcdvlog-core/tests/chronology_epochs_independent.rs"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T010/**"]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/migrations.rs", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test chronology_epochs",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T010\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Replace the current unbounded per-camera offset with a clock epoch bounded to a
counter range inside one capture sequence, so a correction affects only the clips
it was meant to affect.

Today `apply_clock_event(project_id, camera, start_filename, offset_seconds,
label)` applies an offset to every asset of a camera at and after a filename,
forever. A trip where the camera clock was wrong for two days and right afterwards
cannot be expressed.

## Principal invariant

This task is correct when applying one clock epoch changes derived time for every
asset inside its declared counter range and for no asset outside it.

## Inputs and outputs

Inputs:

- The frozen `ClockEpoch` type and `clockEpochSet` / `clockEpochCleared`
  operations.
- Capture sequences and counter assignments from D01-T009.

Outputs:

- Epoch range resolution: which epoch applies to which asset, and how overlaps
  resolve.
- `ProjectCatalog` methods to set and clear an epoch.
- The existing `apply_clock_event` retained as a compatibility wrapper that
  constructs an unbounded epoch, so the current Tauri command and CLI keep working
  until D01-T016 and D01-T017 replace them.

## Frozen decisions

- A range is `[start_counter, end_counter]` inclusive, with a null `end_counter`
  meaning open-ended.
- Overlap resolution is **most specific wins**: a bounded range beats an
  open-ended one; between two bounded ranges the narrower wins; on an exact tie the
  later HLC time wins. This rule is total and deterministic and must be documented
  where a reader of the derived time can find it.
- An epoch never modifies `captured_at_raw`, and never modifies source metadata.
- An epoch applies only within its own capture sequence. It cannot reach across
  sequences, even for the same camera.
- An asset with a null counter is never inside any bounded range. It is affected
  only by an open-ended epoch on its sequence.
- Clearing an epoch restores whatever the remaining epochs imply. It does not
  restore a snapshot.

## Owned paths

The implementer owns the epochs module and its tests. `migrations.rs` is
forbidden — the tables already exist from D01-T007.

## Required interface

```rust
/// The epoch governing this asset, by the most-specific-wins rule.
pub fn governing_epoch<'a>(
    epochs: &'a [ClockEpoch],
    sequence_id: &str,
    counter: Option<u64>,
) -> Option<&'a ClockEpoch>;

impl ProjectCatalog {
    pub fn set_clock_epoch(
        &self,
        project_id: &str,
        capture_sequence_id: &str,
        start_counter: u64,
        end_counter: Option<u64>,
        offset_seconds: i64,
        label: &str,
    ) -> Result<ClockEpoch>;

    pub fn clear_clock_epoch(&self, epoch_id: &str) -> Result<()>;

    /// Assets whose derived time would change, computed before any write.
    pub fn preview_clock_epoch(
        &self,
        project_id: &str,
        capture_sequence_id: &str,
        start_counter: u64,
        end_counter: Option<u64>,
        offset_seconds: i64,
    ) -> Result<Vec<AssetTimeChange>>;
}

pub struct AssetTimeChange {
    pub asset_id: String,
    pub before: Option<String>,
    pub after: Option<String>,
}
```

`preview_clock_epoch` exists here rather than in the UI packet because the
before/after impact must be computed by the same code that applies the change.
A preview computed separately from the application is a preview that lies.

## Acceptance oracle

1. A 200-clip sequence with an epoch bounded to counters 150–160: exactly eleven
   assets change derived time.
2. An epoch bounded 150–160 and an open-ended epoch from 100: assets 150–160 use
   the bounded offset, assets 100–149 and 161+ use the open-ended one.
3. Two bounded epochs 150–160 and 155–157: assets 155–157 use the narrower one.
4. Two identical ranges with different offsets: the later HLC time wins, and the
   result is stable across replay in any delivery order.
5. An epoch on sequence A leaves every asset of sequence B unchanged, including
   when B reuses the same counters.
6. An asset with a null counter is unaffected by a bounded epoch and affected by
   an open-ended one.
7. `preview_clock_epoch` returns exactly the set that `set_clock_epoch` then
   changes — same ids, same before and after values, no write performed by the
   preview.
8. Clearing one of two overlapping epochs yields the times the remaining epoch
   implies.
9. `captured_at_raw` is unchanged for every asset after every operation.
10. The retained `apply_clock_event` wrapper produces the same derived times as
    before this change for an equivalent legacy input.

## Independent test charter

Falsify range containment.

- Build your own 500-asset sequence and record derived times for every asset
  before and after each operation, comparing the full map rather than sampling.
- Attack the boundaries: an epoch whose range is a single counter; a range whose
  start equals its end; a range where `end_counter` is below `start_counter`
  (which must be rejected, not silently swapped); a range beyond the highest
  counter; a range starting at 0.
- Attack overlap resolution with three overlapping epochs and assert the winner
  for every counter, then re-apply the three operations in all six orders and
  assert the same result each time.
- Confirm `preview_clock_epoch` performs no write by hashing the catalog file
  before and after the call.
- Confirm no source media file changed.
- Verify the compatibility wrapper against a catalog created before this change.

## Review charter

Risk domain: an offset that leaks outside its declared range, silently retiming
clips the user did not select.

Questions:

1. Can an epoch affect an asset outside its range or outside its sequence?
2. Is the overlap rule total, deterministic, and documented?
3. Does the preview share the code path that applies the change, or duplicate it?
4. Is an inverted range rejected rather than normalized?
5. Is `captured_at_raw` ever written?
6. Does the compatibility wrapper preserve existing behavior exactly?

Severity rules: range leakage, cross-sequence leakage, a nondeterministic overlap
result, a duplicated preview path, or any write to `captured_at_raw` is blocking.
Error-message wording is non-blocking.

Smallest acceptable remedy: tighten the range predicate and add the boundary case
that exposed the leak.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Clip-level exceptions, which are D01-T011.
- Trip phases and timezones, which are D01-T012.
- Any UI, including the drag interaction and impact display.
- Automatic epoch proposal or detection.
- Removing `apply_clock_event`, which stays until its callers are migrated.

## Stop conditions

Stop and escalate when:

- the most-specific-wins rule produces a result the product owner would find
  surprising in a real case, which is a product decision rather than a bug;
- an epoch appears to need to span sequences to express a real correction;
- preview and apply cannot share a code path without restructuring the solver;
- counter assignment from D01-T009 is missing for assets an epoch must cover;
- the compatibility wrapper cannot preserve existing behavior.

## Commands

```sh
cargo test -p rcdvlog-core --test chronology_epochs
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected result: all tests pass and clippy is clean.

## Handoff

Report the range semantics and overlap rule as implemented, the containment
results for every boundary case, the preview-equals-apply evidence, the
replay-order independence result, and the compatibility wrapper verification. List
any stop condition encountered. The implementer does not approve this task.

## Promotion note

`draft` until D01-T007 publishes `docs/tasks/desktop/CHRONOLOGY_MODEL.md` and
D01-T009 lands counter assignment. The contract editor reconciles the overlap rule
with the frozen `ClockEpoch` payload before promoting.
