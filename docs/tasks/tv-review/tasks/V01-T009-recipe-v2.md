# V01-T009: Recipe v2: energy-weighted robust features

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T009",
  "epic_id": "V01",
  "title": "Recipe v2: energy-weighted robust features",
  "slug": "recipe-v2",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [
    "V01-T005"
  ],
  "principal_invariant": "Indexes and queries built with recipe v2 weight each frame by reference energy and use median-removed ±3-clipped features, and v1 and v2 indexes and caches can never collide.",
  "central_falsification": "Index the same movie window under v1 and v2 and show different index digests and separate cache entries; then show that changing query audio only where the reference is near-silent leaves the v2 score unchanged but changes v1.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "SoundtrackIndex recipe -> features/similarity"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/tv-review/ARCHITECTURE.md",
      "docs/tasks/tv-review/EPIC.md"
    ],
    "implementer": [
      "docs/tasks/tv-review/ARCHITECTURE.md",
      "AGENTS.md",
      "docs/tv-soundtrack-matching.md",
      "docs/tv-review-local-session.md",
      "docs/tv-review-sync.md"
    ],
    "independent_tester": [
      "docs/tasks/tv-review/ARCHITECTURE.md",
      "docs/tv-soundtrack-matching.md"
    ],
    "reviewer": [
      "docs/tasks/tv-review/ARCHITECTURE.md",
      "AGENTS.md",
      "docs/tv-soundtrack-matching.md"
    ],
    "integrator": [
      "docs/tasks/tv-review/EPIC.md",
      "docs/tasks/tv-review/ARCHITECTURE.md",
      "AGENTS.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/tv-review/tasks/V01-T009-recipe-v2.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/tv_soundtrack.rs",
      "core/rcdvlog-core/src/tv_soundtrack_tests.rs",
      "core/rcdvlog-core/src/tv_soundtrack_session.rs",
      "core/rcdvlog-core/src/tv_soundtrack_session_tests.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/v01_t009_recipe_independent.rs"
    ],
    "integrator": [
      "docs/tasks/tv-review/EPIC.md",
      "docs/task-cycles/V01-T009/**"
    ]
  },
  "forbidden_paths": [
    "apps/desktop/**",
    "apps/web/**",
    "apps/server/**",
    "packages/**",
    "core/rcdvlog-server/**",
    "core/rcdvlog-sync/**",
    "core/rcdvlog-core/src/chronology/**",
    "core/rcdvlog-core/src/editplan/**",
    "../data/chronology-audit/**",
    "../data/editorial-review/rc6-passive-sync-2026-11-03/**",
    "../source/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-core tv_soundtrack",
    "cargo test -p rcdvlog-core tv_review_sync",
    "cargo clippy -p rcdvlog-core --all-targets -- -D warnings",
    "cargo fmt --all -- --check",
    "cargo test -p rcdvlog-core --test v01_t009_recipe_independent"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T009\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement §7 behind `recipe: Some(V2)`.

## Principal invariant

This task is correct when indexes and queries built with recipe v2 weight each frame by reference energy and use median-removed ±3-clipped features, and v1 and v2 indexes and caches can never collide.

## Inputs and outputs

Inputs: recipe selection in reference/session spec. Outputs: v2 index (with frame_energy) and v2 scores.

## Frozen decisions

§7 percentiles, clamp, median step, ±3 clip and recipe string; recipe None gives exact v1. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: `core/rcdvlog-core/src/tv_soundtrack.rs`, `core/rcdvlog-core/src/tv_soundtrack_tests.rs`, `core/rcdvlog-core/src/tv_soundtrack_session.rs`, `core/rcdvlog-core/src/tv_soundtrack_session_tests.rs`.
- Independent tester: `core/rcdvlog-core/tests/v01_t009_recipe_independent.rs`.
- Integrator: `docs/tasks/tv-review/EPIC.md`, `docs/task-cycles/V01-T009/**`.
- Forbidden paths always win.

## Required interface

No new public functions beyond T001 fields.

## Acceptance oracle

(1) Digest separation. (2) Silent-reference invariance. (3) A talker over the reference raises the v2 score relative to v1 on the same fixture. (4) All existing v1 tests pass unchanged.

## Independent test charter

Claim to falsify: Index the same movie window under v1 and v2 and show different index digests and separate cache entries; then show that changing query audio only where the reference is near-silent leaves the v2 score unchanged but changes v1. Use the session path with a real tempdir cache and confirm by listing the cache that v1 and v2 runs write distinct entries. Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown, and must never produce an anchor. Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: percentile computed on query instead of reference; recipe not in the digest. Approval: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Changing v1 math.

## Stop conditions

Stop and escalate when:

- a change would alter the serialized bytes or digest of any existing v1 object (§2);
- you need a production path not listed under implementer ownership, a new crate dependency, or a SQLite migration;
- a frozen decision in ARCHITECTURE.md seems wrong or insufficient. Report it and do not redesign;
- the independent oracle would have to be weakened, or thresholds changed, to make tests pass;
- the work no longer fits one focused context.

## Commands

Run these from `vlogcutter/`; all must pass:

```sh
cargo test -p rcdvlog-core tv_soundtrack
cargo test -p rcdvlog-core tv_review_sync
cargo clippy -p rcdvlog-core --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test -p rcdvlog-core --test v01_t009_recipe_independent
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
