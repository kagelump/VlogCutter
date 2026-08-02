# D01-T008: Deterministic chronology solver

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T008",
  "epic_id": "D01",
  "title": "Deterministic chronology solver",
  "slug": "deterministic-chronology-solver",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["D01-T007"],
  "principal_invariant": "Identical chronology facts always produce an identical solution, including a total order with no ties and identically ordered evidence.",
  "central_falsification": "Shuffle every collection inside ChronologyFacts across many seeds, solve each permutation, and confirm the asset order, trip times, confidences, and evidence lists are identical every time.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["ChronologyFacts -> derived chronology solution"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T008-deterministic-chronology-solver.md"],
    "implementer": [
      "core/rcdvlog-core/src/chronology/solver.rs",
      "core/rcdvlog-core/tests/chronology_solver.rs",
      "core/rcdvlog-core/tests/fixtures/chronology/**"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/chronology_solver_independent.rs",
      "core/rcdvlog-core/tests/fixtures/chronology-independent/**"
    ],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T008/**"]
  },
  "forbidden_paths": ["apps/**", "packages/**", "core/rcdvlog-server/**", "core/rcdvlog-core/src/migrations.rs", "core/rcdvlog-core/src/operations.rs", ".github/**"],
  "verification_commands": [
    "cargo test -p rcdvlog-core --test chronology_solver",
    "cargo test --workspace",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T008\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Turn persisted chronology facts into a derived solution: per-asset trip time,
local display time, confidence, evidence, uncertainty, and one total order — as a
pure function, so the desktop, the CLI, and export can never disagree about order.

## Principal invariant

This task is correct when identical chronology facts always produce an identical
solution, including a total order with no ties and identically ordered evidence.

## Inputs and outputs

Inputs:

- `ChronologyFacts` as frozen by `docs/tasks/desktop/CHRONOLOGY_MODEL.md`.

Outputs:

- `core/rcdvlog-core/src/chronology/solver.rs` with `solve`.
- A projection helper that writes the solution's `best_time` and
  `offset_seconds` back to `assets`, which is the only place those columns are
  written.

## Frozen decisions

- The solver is pure: no I/O, no `SystemTime::now`, no environment reads, no
  randomness, no `HashMap` or `HashSet` on any path reaching output.
- The pipeline order follows `PRD.md` §9.5: build capture chains, apply raw
  timestamp hypotheses, apply clock epochs, apply trip-context timezone mappings,
  apply user constraints, detect contradictions, compute best times, produce a
  total order, expose confidence and evidence.
- `PRD.md` §9.5 explicitly permits a deterministic constraint graph plus scored
  candidate times. No optimization engine, no solver library, no iterative
  convergence loop.
- Evidence is ordered and deduplicated by a fixed rule, never by insertion order
  of an unordered collection.
- A missing raw timestamp is not an error. It yields a null trip time, minimum
  confidence, and an order position derived from its sequence and counter.

## Owned paths

The implementer owns the solver and its tests. `migrations.rs` and
`operations.rs` are forbidden: this packet derives, it does not persist. The
independent tester owns separate fixtures so a shared fixture cannot hide a
determinism bug.

## Required interface

```rust
pub struct OrderKey {
    pub trip_time_millis: Option<i64>,
    pub capture_sequence_id: String,
    pub filename_counter: Option<u64>,
    pub filename: String,
    pub asset_id: String,
}

pub struct DisplayTime {
    pub primary: String,
    pub secondary: Option<String>,
    pub timezone: String,
    pub phase_id: Option<String>,
}

pub struct Evidence {
    pub kind: String,
    pub detail: String,
    pub weight: f32,
}

pub struct AssetChronology {
    pub asset_id: String,
    pub raw_time: Option<String>,
    pub trip_time: Option<String>,
    pub display_time: Option<DisplayTime>,
    pub confidence: f32,
    pub evidence: Vec<Evidence>,
    pub uncertainty_seconds: u32,
    pub order_key: OrderKey,
}

pub struct ChronologySolution {
    pub assets: Vec<AssetChronology>,
    pub contradictions: Vec<Contradiction>,
}

/// Pure. Deterministic. Total.
pub fn solve(facts: &ChronologyFacts) -> ChronologySolution;
```

`Contradiction` is defined by D01-T013; this packet returns an empty vector for it
and must not define a competing type.

## Acceptance oracle

1. Solving the same facts twice yields identical solutions, field for field.
2. Solving 32 shuffled permutations of every collection in `ChronologyFacts`
   yields the identical asset order.
3. No two assets have equal `OrderKey`s, even with identical trip times,
   identical filenames, and identical counters.
4. An asset with a null `captured_at_raw` and no exception has a null
   `trip_time`, confidence 0.0, and a stable position.
5. Assets with no epochs and no phases order by raw timestamp, then sequence, then
   counter, then filename, then id.
6. `solve` on empty facts returns an empty solution rather than panicking.
7. A 5,000-asset fixture solves in under 250 ms on the development machine, so
   the review queue stays responsive.
8. `assets.best_time` and `assets.offset_seconds` are written only by the
   projection helper.

## Independent test charter

Falsify determinism, which is where this kind of code actually fails.

- Write your own fixtures. Include: 1,000 assets sharing one raw timestamp to the
  second; two assets identical in every field except id; an asset whose counter is
  `u64::MAX`; filenames differing only by Unicode normalization form.
- Shuffle every collection independently across at least 64 seeds and compare
  full solutions, not just order.
- Property-test `OrderKey` totality: for all pairs, exactly one of `a < b`,
  `a > b`, `a == b` holds, and `a == b` only when the ids are equal.
- Grep the module for `HashMap`, `HashSet`, `iter().collect()` into an unordered
  type, `SystemTime`, `Instant`, `rand`, and `env::`. Any hit on an output path is
  a finding.
- Confirm purity by calling `solve` with the process clock moved forward and
  confirming identical output.

## Review charter

Risk domain: nondeterminism that passes today and reorders a user's trip
tomorrow, and derived values leaking back into persisted authority.

Questions:

1. Is any unordered collection on a path that reaches output?
2. Is `OrderKey` genuinely total, with ties impossible?
3. Does the solver read a clock, the environment, or the filesystem?
4. Does anything other than the projection helper write `best_time` or
   `offset_seconds`?
5. Is the confidence scale defined and documented, or invented per branch?
6. Does the implementation add an optimization loop the frozen decisions forbid?

Severity rules: nondeterminism, a possible tie, impurity, or a second writer of
the projection columns is blocking. Confidence-weight tuning is non-blocking.

Smallest acceptable remedy: replace the unordered collection or add an explicit
total sort, and add the shuffled-permutation test that exposed it.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Contradiction detection, which is D01-T013.
- Capture-sequence proposal, which is D01-T009.
- Timezone phase resolution beyond consuming what D01-T012 provides; until then
  `display_time` is null.
- Any UI, Tauri command, or CLI surface.
- Persisting anything other than the projection columns.

## Stop conditions

Stop and escalate when:

- the frozen `ChronologyFacts` lacks a field the pipeline needs;
- totality cannot be achieved without inventing a tie-break the model does not
  supply;
- the 250 ms budget cannot be met without caching, which would make the solver
  impure;
- clock epochs and trip phases interact in a way `CHRONOLOGY_MODEL.md` does not
  specify;
- determinism requires changing how facts are persisted, which is D01-T007's
  territory.

## Commands

```sh
cargo test -p rcdvlog-core --test chronology_solver
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
grep -n "HashMap\|HashSet\|SystemTime\|Instant\|rand::" core/rcdvlog-core/src/chronology/solver.rs
```

Expected result: tests pass, clippy is clean, and the grep returns nothing on an
output path.

## Handoff

Report the pipeline as implemented, the determinism evidence including seed count,
the totality property-test result, the 5,000-asset timing, and confirmation that
the projection helper is the sole writer of `best_time` and `offset_seconds`. List
any stop condition encountered. The implementer does not approve this task.

## Promotion note

`draft` because `architecture_decisions_frozen` would be false: `ChronologyFacts`
and the fact model are frozen by D01-T007. The contract editor promotes this
packet after `docs/tasks/desktop/CHRONOLOGY_MODEL.md` is published, reconciling
the interface above with what that document actually froze.
