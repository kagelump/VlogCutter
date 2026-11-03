# V01-T007: Offset consensus module and CLI

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T007",
  "epic_id": "V01",
  "title": "Offset consensus module and CLI",
  "slug": "offset-consensus",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [
    "V01-T004"
  ],
  "principal_invariant": "Consensus emits a segment only for ≥3 supporting queries (≥2 Matched) at one offset within ±0.08 s, splits at any query Matched at another offset, and rejects segments beyond max drift.",
  "central_falsification": "Synthetic evidence: constant offset with 3 weak plus 2 matched windows gives one segment; the same with the second half shifted 30 s gives two segments and a boundary; random offsets give none; a 3000 ppm drift is rejected.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "complete session evidence -> SoundtrackConsensus"
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
      "docs/tasks/tv-review/tasks/V01-T007-offset-consensus.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/tv_soundtrack_consensus.rs",
      "core/rcdvlog-core/src/tv_soundtrack_consensus_tests.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/v01_t007_consensus_independent.rs"
    ],
    "integrator": [
      "apps/cli/src/tv_review_commands.rs",
      "apps/cli/src/main.rs",
      "apps/cli/tests/**",
      "docs/tasks/tv-review/EPIC.md",
      "docs/task-cycles/V01-T007/**"
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
    "cargo test -p rcdvlog-core --test v01_t007_consensus_independent",
    "cargo test -p rcdvlog-cli"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T007\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement §8 and expose `tv-review soundtrack-session-consensus --plan P --cache C`. The integrator wires the CLI.

## Principal invariant

This task is correct when consensus emits a segment only for ≥3 supporting queries (≥2 Matched) at one offset within ±0.08 s, splits at any query Matched at another offset, and rejects segments beyond max drift.

## Inputs and outputs

Inputs: plan plus cache (complete). Outputs: SoundtrackConsensus JSON in the standard CLI envelope.

## Frozen decisions

All §8 constants and rules; boundaries reuse SyncBoundary; error code search_incomplete for an incomplete search. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: `core/rcdvlog-core/src/tv_soundtrack_consensus.rs`, `core/rcdvlog-core/src/tv_soundtrack_consensus_tests.rs`.
- Independent tester: `core/rcdvlog-core/tests/v01_t007_consensus_independent.rs`.
- Integrator: `apps/cli/src/tv_review_commands.rs`, `apps/cli/src/main.rs`, `apps/cli/tests/**`, `docs/tasks/tv-review/EPIC.md`, `docs/task-cycles/V01-T007/**`.
- Forbidden paths always win.

## Required interface

`pub fn consensus(plan: &SoundtrackSessionPlan, cache: &Path, cancel: &dyn Fn() -> bool) -> Result<SoundtrackConsensus>`.

## Acceptance oracle

The four falsification cases plus: an incomplete search gives an error; output is deterministic across two runs (byte-identical).

## Independent test charter

Claim to falsify: Synthetic evidence: constant offset with 3 weak plus 2 matched windows gives one segment; the same with the second half shifted 30 s gives two segments and a boundary; random offsets give none; a 3000 ppm drift is rejected. Go through the CLI binary (`cargo run -p rcdvlog-cli -- tv-review soundtrack-session-consensus`) against a tempdir cache produced by real run_batch calls. Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown, and must never produce an anchor. Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: a pause or seek being glued into one segment. Ask: does a single Matched rival offset always split? Approval: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Mapper changes (T008).

## Stop conditions

Stop and escalate when:

- a change would alter the serialized bytes or digest of any existing v1 object (§2);
- you need a production path not listed under implementer ownership, a new crate dependency, or a SQLite migration;
- a frozen decision in ARCHITECTURE.md seems wrong or insufficient. Report it and do not redesign;
- the independent oracle would have to be weakened, or thresholds changed, to make tests pass;
- the work no longer fits one focused context.
- the CLI needs more than one new subcommand, or changes to existing command output.

## Commands

Run these from `vlogcutter/`; all must pass:

```sh
cargo test -p rcdvlog-core tv_soundtrack
cargo test -p rcdvlog-core tv_review_sync
cargo clippy -p rcdvlog-core --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test -p rcdvlog-core --test v01_t007_consensus_independent
cargo test -p rcdvlog-cli
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
