# D01-T013: Event-order constraints and contradiction reporting

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T013",
  "epic_id": "D01",
  "title": "Event-order constraints and contradiction reporting",
  "slug": "constraints-and-contradiction-reporting",
  "status": "draft",
  "task_class": "implementation",
  "depends_on": ["D01-T011", "D01-T012"],
  "principal_invariant": "A hard contradiction blocks the change that would create it and names every conflicting piece of evidence.",
  "central_falsification": "Assert that A precedes B as hard knowledge, then attempt a clock epoch that would place B before A, and confirm the apply is refused with both the constraint and the epoch named in the report.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["constraint graph -> contradiction report"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T013-constraints-and-contradiction-reporting.md"],
    "implementer": [
      "core/rcdvlog-core/src/chronology/contradictions.rs",
      "core/rcdvlog-core/tests/chronology_contradictions.rs"
    ],
    "independent_tester": ["core/rcdvlog-core/tests/chronology_contradictions_independent.rs"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T013/**"]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/migrations.rs", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test chronology_contradictions",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T013\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Let the user state ordering knowledge that has nothing to do with clocks — "the
breakfast clips came before the departure clips" — and report when the accumulated
evidence contradicts itself, blocking changes that would make the chronology
incoherent.

This is also where before/after placement gets its domain meaning: placing clip B
after clip A is an event-order constraint, not a manual sort.

## Principal invariant

This task is correct when a hard contradiction blocks the change that would create
it and names every conflicting piece of evidence.

## Inputs and outputs

Inputs:

- The frozen `EventOrderConstraint` type, `ConstraintStrength`, and the
  `eventOrderConstraintSet` / `eventOrderConstraintCleared` operations.
- Derived times from epochs (D01-T010), exceptions (D01-T011), and phases
  (D01-T012).

Outputs:

- The `Contradiction` type, which D01-T008's solution declared and this packet
  defines.
- Cycle detection over the hard-constraint graph.
- Interval-conflict detection between constraints and derived times.
- Blocking behavior: an operation that would create a hard contradiction is
  refused before it is committed.
- `ProjectCatalog` methods to set and clear a constraint, and to report current
  contradictions.

## Frozen decisions

- `hard` blocks; `soft` reports only. Two strengths, no third.
- A contradiction is reported against the user's evidence, never resolved
  silently. It is never resolved by discarding a fact, weakening a constraint, or
  adjusting a time the user did not ask to adjust.
- Blocking happens before commit. A refused operation leaves no row and no
  conflict record behind — refusal is not a conflict, it is a validation failure.
- Detection is deterministic: the same facts produce the same contradictions in
  the same order, with the same asset ids in the same order inside each one.
- Cycle detection covers the transitive closure of hard constraints. A three-node
  cycle is as much a cycle as a two-node one.
- Soft constraints never affect the total order. They inform confidence and
  produce reports. If a soft constraint could reorder clips, it would be a hard
  constraint with extra steps.
- A constraint between an asset and itself is rejected at validation, not stored
  and then reported.

## Owned paths

The implementer owns the contradictions module and its tests. Epoch, exception,
and phase modules are read but not modified; needing to modify them is a stop
condition.

## Required interface

```rust
pub enum ContradictionKind {
    /// Hard constraints form a cycle.
    Cycle,
    /// A hard constraint disagrees with derived times.
    IntervalConflict,
    /// A clip exception places a clip against a hard constraint.
    ExceptionConflict,
}

pub struct Contradiction {
    pub kind: ContradictionKind,
    pub strength: ConstraintStrength,
    pub asset_ids: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub message: String,
}

/// Deterministic. Ordered by kind, then by the sorted asset id list.
pub fn detect(facts: &ChronologyFacts, solved: &[AssetChronology]) -> Vec<Contradiction>;

impl ProjectCatalog {
    /// Refuses with `ChronologyError::HardContradiction` when the constraint
    /// would create one. Nothing is written in that case.
    pub fn set_event_order_constraint(
        &self,
        project_id: &str,
        earlier_asset_id: &str,
        later_asset_id: &str,
        strength: ConstraintStrength,
        source: &str,
    ) -> Result<EventOrderConstraint>;

    pub fn clear_event_order_constraint(&self, constraint_id: &str) -> Result<()>;

    pub fn contradictions(&self, project_id: &str) -> Result<Vec<Contradiction>>;

    /// The contradictions a candidate epoch would create, computed without
    /// writing. Used by the calibration impact preview.
    pub fn preview_contradictions(
        &self,
        project_id: &str,
        candidate: &CandidateChange,
    ) -> Result<Vec<Contradiction>>;
}
```

`ChronologyError::HardContradiction` carries the contradiction list, so a caller
can show the user what blocked them without a second query.

## Acceptance oracle

1. Hard `A before B` plus an epoch that would place B before A: the epoch apply is
   refused, the error carries an `IntervalConflict` naming both A and B, and its
   evidence names both the constraint and the epoch.
2. The refused apply wrote nothing: no epoch row, no conflict row, and derived
   times are unchanged.
3. Hard `A before B` and hard `B before A`: the second constraint is refused with a
   `Cycle`.
4. Hard `A before B`, `B before C`, `C before A`: the third is refused with a
   `Cycle` naming all three assets.
5. Soft `A before B` plus an epoch placing B before A: the epoch applies, and
   `contradictions` reports one soft `IntervalConflict`.
6. An exception placing a clip against a hard constraint is refused with an
   `ExceptionConflict`.
7. A constraint from an asset to itself is rejected with a stable error code.
8. A constraint naming an asset that does not exist is rejected before any write.
9. `detect` is deterministic across 32 shuffles of the constraint list, including
   the order of `asset_ids` inside each contradiction.
10. `preview_contradictions` returns exactly what applying the candidate would
    produce, and writes nothing — verified by hashing the catalog file.
11. Clearing a constraint that participated in a soft contradiction removes that
    contradiction from the report.
12. A project with 1,000 hard constraints across 5,000 assets detects in under
    250 ms, so the calibration preview stays interactive.

## Independent test charter

Falsify the blocking claim, which is the one users will rely on.

- Build your own graphs: a 50-node hard chain; a cycle closed by the 50th
  constraint; two disjoint components where one has a cycle; a node with 20
  incoming and 20 outgoing hard constraints; a graph mixing hard and soft edges
  between the same pair.
- For every refusal case, assert the catalog is byte-identical before and after the
  attempt. A refusal that leaves a partial write is worse than no refusal.
- Attack transitivity: assert `A before B` and `B before C` as hard, then attempt
  an epoch placing C before A. This must be refused even though no single
  constraint mentions both C and A.
- Attack the mixed case: a soft edge that agrees with derived times and a hard edge
  that disagrees, and confirm only the hard one blocks.
- Attack determinism by shuffling constraints across 64 seeds and comparing the
  full contradiction list including inner asset id order.
- Confirm `preview_contradictions` and the real apply agree by running both and
  comparing.
- Confirm no contradiction is resolved silently: after every refused attempt, every
  original fact is still present and unmodified.

## Review charter

Risk domain: a blocking rule that either fails to block or blocks partially,
leaving the catalog in a state the user did not ask for.

Questions:

1. Does a refused operation leave the catalog completely unchanged?
2. Is the transitive closure of hard constraints considered, or only direct edges?
3. Does any code path resolve a contradiction by modifying or dropping a fact?
4. Do soft constraints affect the total order anywhere?
5. Is the contradiction list deterministic, including inner asset id order?
6. Does the preview share the detection path with the real apply?
7. Is a self-constraint rejected at validation rather than stored?

Severity rules: a partial write on refusal, a missed transitive cycle, a silent
resolution, or a soft constraint affecting order is blocking. Message wording and
performance within budget are non-blocking.

Smallest acceptable remedy: move the detection ahead of the write, or extend
detection to the transitive closure, plus the graph case that exposed the gap.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Any UI, including the before/after placement interaction, which is D01-T016.
- Suggesting resolutions or auto-repairing contradictions.
- Constraints between phases or sequences rather than assets.
- Weighted or probabilistic constraints.
- Changing epoch, exception, or phase behavior.

## Stop conditions

Stop and escalate when:

- blocking cannot be implemented before commit without restructuring how
  operations are committed;
- a real case needs a strength between hard and soft;
- the transitive closure cannot be computed within the performance budget for a
  realistic project;
- a contradiction is detectable but cannot be attributed to specific evidence,
  making the report unactionable;
- detection requires changing the solver, which this packet does not own.

## Commands

```sh
cargo test -p rcdvlog-core --test chronology_contradictions
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected result: all tests pass and clippy is clean.

## Handoff

Report the detection algorithm and its complexity, the blocking behavior with
evidence that refused operations write nothing, the transitive-cycle results, the
soft-versus-hard distinction results, the determinism evidence, the
preview-equals-apply verification, and the 5,000-asset timing. List any stop
condition encountered. The implementer does not approve this task.

## Promotion note

`draft` until D01-T007 publishes `docs/tasks/desktop/CHRONOLOGY_MODEL.md` and
D01-T011 and D01-T012 land. The contract editor confirms `Contradiction` as
defined here matches what D01-T008's `ChronologySolution` expects before promoting.
