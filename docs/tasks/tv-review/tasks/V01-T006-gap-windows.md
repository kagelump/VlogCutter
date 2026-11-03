# V01-T006: Gap-placed query windows in session plans

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T006",
  "epic_id": "V01",
  "title": "Gap-placed query windows in session plans",
  "slug": "gap-windows",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [
    "V01-T005"
  ],
  "principal_invariant": "A plan with a speech mask contains, besides the unchanged grid queries, exactly one speech_gap query per unmasked gap of at least 4 s, sized min(gap, 20 s) and centred, chosen longest-first under the quota.",
  "central_falsification": "Give a mask with known gaps (3 s, 4 s, 7 s, 45 s) and assert the exact sample ranges of appended queries; an empty mask must reproduce the main-branch plan digest.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "SoundtrackSessionPlan::new query generation"
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
      "docs/tasks/tv-review/tasks/V01-T006-gap-windows.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/tv_soundtrack_session.rs",
      "core/rcdvlog-core/src/tv_soundtrack_session_tests.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/v01_t006_gaps_independent.rs"
    ],
    "integrator": [
      "docs/tasks/tv-review/EPIC.md",
      "docs/task-cycles/V01-T006/**"
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
    "cargo test -p rcdvlog-core --test v01_t006_gaps_independent"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T006\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement the §6 gap windows, so that windows land where the editor is silent.

## Principal invariant

This task is correct when a plan with a speech mask contains, besides the unchanged grid queries, exactly one speech_gap query per unmasked gap of at least 4 s, sized min(gap, 20 s) and centred, chosen longest-first under the quota.

## Inputs and outputs

Inputs: spec with speech_mask. Outputs: plan queries with an `origin` field.

## Frozen decisions

§6 sizing, centring, quota and tie-break rules; grid query origin None. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: `core/rcdvlog-core/src/tv_soundtrack_session.rs`, `core/rcdvlog-core/src/tv_soundtrack_session_tests.rs`.
- Independent tester: `core/rcdvlog-core/tests/v01_t006_gaps_independent.rs`.
- Integrator: `docs/tasks/tv-review/EPIC.md`, `docs/task-cycles/V01-T006/**`.
- Forbidden paths always win.

## Required interface

No new public functions.

## Acceptance oracle

(1) The exact ranges in the falsification case. (2) Gaps at the recording edges are handled (clamped to the recording). (3) Quota overflow keeps the longest gaps. (4) Empty mask gives a plan digest identical to main.

## Independent test charter

Claim to falsify: Give a mask with known gaps (3 s, 4 s, 7 s, 45 s) and assert the exact sample ranges of appended queries; an empty mask must reproduce the main-branch plan digest. Write your own interval sets, including adjacent and overlapping mask intervals and a gap of exactly 4.0 s. Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown, and must never produce an anchor. Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: off-by-one sample errors and overlapping duplicates. Approval: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Changing grid queries.

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
cargo test -p rcdvlog-core --test v01_t006_gaps_independent
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
