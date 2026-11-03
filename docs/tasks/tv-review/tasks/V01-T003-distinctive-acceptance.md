# V01-T003: Distinctive-peak acceptance in match_pcm

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T003",
  "epic_id": "V01",
  "title": "Distinctive-peak acceptance in match_pcm",
  "slug": "distinctive-acceptance",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "V01-T002"
  ],
  "principal_invariant": "With policy.distinctive set, a window whose best peak passes temporal support and all three §4 distinctive thresholds is Matched with reason distinctive_peak_low_similarity and confidence min(1, 0.5+margin); with it unset, every report is byte-identical to v1.",
  "central_falsification": "Generate a low-SNR query (cosine ≈0.6) with a unique true position and a query of identical SNR over audio repeated twice in the reference; the first must match, the second must not; rerun both with distinctive=None and compare bytes against main.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "SoundtrackPolicy -> match_pcm acceptance"
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
      "docs/tasks/tv-review/tasks/V01-T003-distinctive-acceptance.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/tv_soundtrack.rs",
      "core/rcdvlog-core/src/tv_soundtrack_tests.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/v01_t003_distinctive_independent.rs"
    ],
    "integrator": [
      "docs/tasks/tv-review/EPIC.md",
      "docs/task-cycles/V01-T003/**"
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
    "cargo test -p rcdvlog-core --test v01_t003_distinctive_independent"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T003\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement §4: robust z-score over coarse scores, the distinctive acceptance branch, its reason string and confidence rule.

## Principal invariant

This task is correct when with policy.distinctive set, a window whose best peak passes temporal support and all three §4 distinctive thresholds is Matched with reason distinctive_peak_low_similarity and confidence min(1, 0.5+margin); with it unset, every report is byte-identical to v1.

## Inputs and outputs

Inputs: coarse score vector, peaks, policy. Outputs: `distinctiveness`, state/reason/candidates.

## Frozen decisions

Acceptance order in §4; MAD constant 1.4826; z None when MAD = 0 fails; confidence formula; old path untouched. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: `core/rcdvlog-core/src/tv_soundtrack.rs`, `core/rcdvlog-core/src/tv_soundtrack_tests.rs`.
- Independent tester: `core/rcdvlog-core/tests/v01_t003_distinctive_independent.rs`.
- Integrator: `docs/tasks/tv-review/EPIC.md`, `docs/task-cycles/V01-T003/**`.
- Forbidden paths always win.

## Required interface

No new public functions.

## Acceptance oracle

(1) Unique peak at 0.6 with margin 0.35 → Matched. (2) Margin 0.1 → not matched. (3) Repeated content → ambiguous/unmatched. (4) Flat noise → z small → unmatched. (5) distinctive None → identical bytes to main for all existing tests.

## Independent test charter

Claim to falsify: Generate a low-SNR query (cosine ≈0.6) with a unique true position and a query of identical SNR over audio repeated twice in the reference; the first must match, the second must not; rerun both with distinctive=None and compare bytes against main. Build your own reference and query signals, and include a periodic signal (e.g. a metronome) as a false-match bait. Assert that no candidate is ever emitted at a position other than the true one. Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown, and must never produce an anchor. Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: false positives on real rooms. Ask: can z pass on near-constant score vectors? Is the temporal-support check still applied before the distinctive branch? Approval: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Cross-reference margin (T004).
- Changing SyncPolicy.minimum_confidence.

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
cargo test -p rcdvlog-core --test v01_t003_distinctive_independent
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
