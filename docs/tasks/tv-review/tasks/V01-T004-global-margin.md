# V01-T004: Cross-reference margin in session evidence

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T004",
  "epic_id": "V01",
  "title": "Cross-reference margin in session evidence",
  "slug": "global-margin",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [
    "V01-T003"
  ],
  "principal_invariant": "With policy.distinctive set, a session query emits anchors only when its matched peak beats every merged peak at a different movie position across all references by the applicable margin.",
  "central_falsification": "Build a movie whose content repeats in two references more than 120 s apart, so each reference alone reports Matched; the session evidence for that query must be ambiguous with zero anchors.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "per-reference reports -> SessionQueryEvidence state/anchors"
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
      "docs/tasks/tv-review/tasks/V01-T004-global-margin.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/tv_soundtrack_session.rs",
      "core/rcdvlog-core/src/tv_soundtrack_session_tests.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/v01_t004_global_margin_independent.rs"
    ],
    "integrator": [
      "docs/tasks/tv-review/EPIC.md",
      "docs/task-cycles/V01-T004/**"
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
    "cargo test -p rcdvlog-core --test v01_t004_global_margin_independent"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T004\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement §5 in `evidence_page`, so that a repeat in a different reference window cannot produce a false unique anchor.

## Principal invariant

This task is correct when with policy.distinctive set, a session query emits anchors only when its matched peak beats every merged peak at a different movie position across all references by the applicable margin.

## Inputs and outputs

Inputs: cached reports for a query across all references. Outputs: session query state and anchors.

## Frozen decisions

§5 rules; a 0.256 s 'different position' threshold; v1 behaviour when distinctive is None. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: `core/rcdvlog-core/src/tv_soundtrack_session.rs`, `core/rcdvlog-core/src/tv_soundtrack_session_tests.rs`.
- Independent tester: `core/rcdvlog-core/tests/v01_t004_global_margin_independent.rs`.
- Integrator: `docs/tasks/tv-review/EPIC.md`, `docs/task-cycles/V01-T004/**`.
- Forbidden paths always win.

## Required interface

No new public functions.

## Acceptance oracle

(1) A repeat across references gives ambiguous with no anchors. (2) A single occurrence spanning two overlapping references is still matched (both references see the same position). (3) distinctive None gives page bytes identical to main.

## Independent test charter

Claim to falsify: Build a movie whose content repeats in two references more than 120 s apart, so each reference alone reports Matched; the session evidence for that query must be ambiguous with zero anchors. Use real WAV files in a tempdir and a synthetic movie built with ffmpeg via the existing test helpers, going through the plan → run_batch → evidence_page public API. Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown, and must never produce an anchor. Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: overlapping references double-counting the same peak as a rival. Ask: are same-position peaks from overlapping references merged before the margin check? Approval: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Consensus (T007).

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
cargo test -p rcdvlog-core --test v01_t004_global_margin_independent
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
