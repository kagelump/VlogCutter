# D01-T014: Chronology explain and review-queue stability

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T014",
  "epic_id": "D01",
  "title": "Chronology explain and review-queue stability",
  "slug": "chronology-explain-and-queue-stability",
  "status": "draft",
  "task_class": "implementation",
  "depends_on": ["D01-T013"],
  "principal_invariant": "A chronology change that reorders the queue leaves the reviewer on the item they were about to review next.",
  "central_falsification": "Capture the next review item in a 500-clip project, apply an epoch that reorders half the queue, and confirm the next item is unchanged and its explanation names every correction that moved it.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["chronology solution -> review queue position"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T014-chronology-explain-and-queue-stability.md"],
    "implementer": [
      "core/rcdvlog-core/src/chronology/explain.rs",
      "core/rcdvlog-core/src/chronology/queue.rs",
      "core/rcdvlog-core/tests/chronology_explain.rs"
    ],
    "independent_tester": ["core/rcdvlog-core/tests/chronology_queue_independent.rs"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T014/**"]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/migrations.rs", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test chronology_explain",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T014\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Make every derived time explainable, and make a reorder survivable: after a
correction that moves many clips, the reviewer resumes exactly where they were
going, not wherever the new order happens to put the cursor.

`PRD.md` §12 identifies this as the defense against the disorientation a
chronology model inherently causes.

## Principal invariant

This task is correct when a chronology change that reorders the queue leaves the
reviewer on the item they were about to review next.

## Inputs and outputs

Inputs:

- The full solution from D01-T008 through D01-T013: times, confidence, evidence,
  contradictions.

Outputs:

- An explanation for one asset: raw time, trip time, display time, confidence,
  uncertainty, and every correction that applied, in the order applied.
- A review-queue position type that survives reordering, plus capture and restore.

## Frozen decisions

- The explanation is derived, never stored. Storing it would let it drift from the
  solution it describes.
- The explanation names corrections by their identity — epoch id, exception asset
  id, phase id, constraint id — so a caller can navigate to the thing that caused
  the time.
- Queue position is anchored to an asset id, not an index. An index is meaningless
  after a reorder, which is the entire problem.
- When the anchored asset is no longer in the queue — removed, or filtered out —
  restore falls back to the nearest surviving neighbour by the pre-change order.
  This matches the `nextSelection` rule frozen in D01-T003, and the two must agree.
- Capture happens before the change is applied, not after. Capturing afterwards
  captures the damage.
- Explanation output is deterministic and its evidence order matches the
  solution's.

## Owned paths

The implementer owns the explain and queue modules and their tests. The solver and
the fact modules are read-only here.

## Required interface

```rust
pub struct ChronologyExplanation {
    pub asset_id: String,
    pub raw_time: Option<String>,
    pub trip_time: Option<String>,
    pub display_time: Option<DisplayTime>,
    pub confidence: f32,
    pub uncertainty_seconds: u32,
    pub applied_corrections: Vec<AppliedCorrection>,
    pub contradictions: Vec<Contradiction>,
}

pub enum AppliedCorrection {
    CaptureSequence { sequence_id: String, label: String },
    ClockEpoch { epoch_id: String, offset_seconds: i64, label: String },
    ClipException { trip_time: String, reason: String },
    TripPhase { phase_id: String, timezone: String, label: String },
    EventOrder { constraint_id: String, strength: ConstraintStrength },
}

pub struct QueuePosition {
    pub anchor_asset_id: String,
    pub ordered_predecessors: Vec<String>,
}

impl ProjectCatalog {
    pub fn explain_chronology(&self, asset_id: &str) -> Result<ChronologyExplanation>;

    /// Call before applying a change.
    pub fn capture_queue_position(&self, project_id: &str, current_asset_id: &str)
        -> Result<QueuePosition>;

    /// Call after applying a change. Returns the asset to resume on.
    pub fn restore_queue_position(&self, project_id: &str, position: &QueuePosition)
        -> Result<String>;
}
```

`ordered_predecessors` is bounded to the ten assets preceding the anchor, which is
enough to find a surviving neighbour without carrying the whole queue.

## Acceptance oracle

1. A 500-clip project, next item captured, an epoch applied that reorders 250
   clips: `restore_queue_position` returns the same asset id.
2. The same, where the anchored asset was removed during the change: restore
   returns the nearest surviving predecessor.
3. The same, where the anchored asset and all ten predecessors were removed:
   restore returns a valid asset id in the queue and does not error.
4. An asset affected by a sequence, an epoch, an exception, a phase, and a
   constraint has all five in `applied_corrections`, in application order.
5. An asset affected by nothing has an empty `applied_corrections` and a
   `trip_time` equal to its `raw_time`.
6. An asset whose time came from an exception shows the `ClipException` correction
   with the user's reason, and its `raw_time` still shows the camera timestamp.
7. `explain_chronology` on an asset in a project with a hard contradiction includes
   that contradiction.
8. Explanations are deterministic across 32 repeated calls.
9. `explain_chronology` on an unknown asset id returns a stable error code, not a
   panic.
10. Capturing a position, applying a change that reorders nothing, and restoring
    returns the same asset — the no-op case must not move the reviewer either.

## Independent test charter

Falsify queue stability under the changes most likely to break it.

- Build your own 1,000-clip project across four capture sequences.
- Apply, one at a time, each of: an epoch moving a range across the anchor; an
  epoch moving the anchor itself; an exception on the anchor; a phase change; a
  constraint that reorders around the anchor; a sequence split containing the
  anchor. Assert resumption for each.
- Attack the fallback: remove the anchor and its predecessors in the same change.
  Remove every asset before the anchor. Remove every asset after it.
- Attack the ordering: apply a change that reverses the entire queue and confirm
  restore still lands on the anchor.
- Confirm capture-then-restore with no intervening change is an identity.
- Cross-check the fallback rule against `apps/desktop/src/review/order.ts`
  `nextSelection` from D01-T003, using the same scenario in both, and report any
  disagreement. Two different fallback rules in one product is a defect even if each
  is individually reasonable.
- Assert `applied_corrections` completeness by constructing an asset touched by all
  five correction kinds and comparing against a hand-computed expectation.

## Review charter

Risk domain: the reviewer losing their place, which is the specific user harm
`PRD.md` §12 exists to prevent, and an explanation that omits the correction the
user needs to see.

Questions:

1. Is position anchored to an asset id rather than an index anywhere?
2. Is capture required before the change, and is that enforced or merely
   documented?
3. Does the fallback rule match `nextSelection` in the desktop review module?
4. Can `restore_queue_position` return an asset not in the queue, or error on a
   queue that still has assets?
5. Does `applied_corrections` omit any correction kind that affected the time?
6. Is the explanation derived rather than stored?

Severity rules: losing the position, returning an invalid asset, a fallback rule
that disagrees with the desktop module, or an omitted correction is blocking.
Predecessor-window size and message wording are non-blocking.

Smallest acceptable remedy: correct the anchor or fallback logic and add the
removal scenario that exposed it.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Any UI. The desktop consumes this in D01-T015 and D01-T016.
- The CLI surface, which is D01-T017.
- Changing the solver or any fact module.
- Persisting review position across restarts, which already exists.
- Suggesting corrections.

## Stop conditions

Stop and escalate when:

- the fallback rule cannot be reconciled with D01-T003's `nextSelection` without
  changing one of them, which needs a decision about which is authoritative;
- capture-before-change cannot be enforced at the API level, leaving correctness
  dependent on caller discipline;
- an explanation cannot attribute a derived time to specific evidence, meaning the
  solver is not recording enough;
- the ten-predecessor window proves insufficient for a realistic removal case.

## Commands

```sh
cargo test -p rcdvlog-core --test chronology_explain
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected result: all tests pass and clippy is clean.

## Handoff

Report the position anchoring and fallback rule as implemented, the resumption
results for every change kind, the fallback results for the removal cases, the
cross-check against `nextSelection` with any disagreement stated, and the
explanation completeness evidence. List any stop condition encountered. The
implementer does not approve this task.

## Promotion note

`draft` until D01-T013 completes, since the explanation must carry
`Contradiction` as that packet defines it. The contract editor also verifies the
`nextSelection` rule from D01-T003 before promoting, so the two fallback rules are
reconciled by design rather than discovered in conflict during implementation.
