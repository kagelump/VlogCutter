# V01-T008: Consensus-gated interpolation in the sync mapper

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T008",
  "epic_id": "V01",
  "title": "Consensus-gated interpolation in the sync mapper",
  "slug": "consensus-interpolation",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "V01-T007"
  ],
  "principal_invariant": "An utterance between anchors farther apart than max_anchor_gap_seconds maps as ConsensusInterpolated only when both anchors and the whole utterance lie in one supplied consensus segment with no boundary between; all other mappings are unchanged.",
  "central_falsification": "Anchors 60 s apart inside a segment give ConsensusInterpolated with the §9 uncertainty; the same without a segment gives Unknown; with a boundary inside the gap gives Unknown; an utterance past the last supporting window gives Unknown.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "SyncRequest.consensus_segments -> map_slice"
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
      "docs/tasks/tv-review/tasks/V01-T008-consensus-interpolation.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/tv_review_sync.rs",
      "core/rcdvlog-core/src/tv_review_sync_tests.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/v01_t008_mapping_independent.rs"
    ],
    "integrator": [
      "apps/cli/src/tv_review_commands.rs",
      "apps/cli/tests/**",
      "docs/tasks/tv-review/EPIC.md",
      "docs/task-cycles/V01-T008/**"
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
    "cargo test -p rcdvlog-core --test v01_t008_mapping_independent",
    "cargo test -p rcdvlog-cli"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T008\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement §9, and add `--consensus FILE` to `soundtrack-session-sync`; the integrator wires the CLI flag.

## Principal invariant

This task is correct when an utterance between anchors farther apart than max_anchor_gap_seconds maps as ConsensusInterpolated only when both anchors and the whole utterance lie in one supplied consensus segment with no boundary between; all other mappings are unchanged.

## Inputs and outputs

Inputs: SyncRequest with consensus_segments. Outputs: SyncSlice state, uncertainty and diagnostics.

## Frozen decisions

§9 rules, uncertainty formula and diagnostic string; no extrapolation; minimum_confidence unchanged. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: `core/rcdvlog-core/src/tv_review_sync.rs`, `core/rcdvlog-core/src/tv_review_sync_tests.rs`.
- Independent tester: `core/rcdvlog-core/tests/v01_t008_mapping_independent.rs`.
- Integrator: `apps/cli/src/tv_review_commands.rs`, `apps/cli/tests/**`, `docs/tasks/tv-review/EPIC.md`, `docs/task-cycles/V01-T008/**`.
- Forbidden paths always win.

## Required interface

No new public functions; the new SyncState variant from T001.

## Acceptance oracle

The four falsification cases, plus: an empty consensus_segments list gives a SyncReport byte-identical to main for all existing tests.

## Independent test charter

Claim to falsify: Anchors 60 s apart inside a segment give ConsensusInterpolated with the §9 uncertainty; the same without a segment gives Unknown; with a boundary inside the gap gives Unknown; an utterance past the last supporting window gives Unknown. Construct SyncRequest JSON by hand with segments and anchors, and map via the public `map` API and via the CLI `soundtrack-session-sync --consensus`. Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown, and must never produce an anchor. Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: consensus silently overriding contradicting anchors. Ask: what happens if anchors inside the segment disagree with the segment offset beyond residual tolerance? (Expected: the existing conflict path, not consensus.) Approval: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Inferring segments in the mapper.

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
cargo test -p rcdvlog-core --test v01_t008_mapping_independent
cargo test -p rcdvlog-cli
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
