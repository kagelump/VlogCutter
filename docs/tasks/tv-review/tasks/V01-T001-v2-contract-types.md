# V01-T001: Freeze v2 contract types with v1 byte compatibility

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T001",
  "epic_id": "V01",
  "title": "Freeze v2 contract types with v1 byte compatibility",
  "slug": "v2-contract-types",
  "status": "ready",
  "task_class": "foundation",
  "depends_on": [],
  "principal_invariant": "Every new v2 type, field, recipe constant and SyncState variant named in ARCHITECTURE.md §3–§9 exists, and every existing v1 spec, plan, index, report and sync request round-trips through serde with byte-identical JSON and unchanged digests.",
  "central_falsification": "Re-serialise the preserved RC6 run's plan.json, spec.json, target.json and evidence.stdout.json data through the rebuilt types and compare bytes; then set each new field and prove it appears in the output.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "serde JSON <-> soundtrack/session/sync DTOs"
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
      "docs/tasks/tv-review/tasks/V01-T001-v2-contract-types.md"
    ],
    "implementer": [
      "core/rcdvlog-core/src/tv_soundtrack.rs",
      "core/rcdvlog-core/src/tv_soundtrack_tests.rs",
      "core/rcdvlog-core/src/tv_soundtrack_session.rs",
      "core/rcdvlog-core/src/tv_soundtrack_session_tests.rs",
      "core/rcdvlog-core/src/tv_review_sync.rs",
      "core/rcdvlog-core/src/tv_review_sync_tests.rs",
      "core/rcdvlog-core/src/tv_soundtrack_consensus.rs",
      "core/rcdvlog-core/src/tv_soundtrack_consensus_tests.rs"
    ],
    "independent_tester": [
      "core/rcdvlog-core/tests/v01_t001_compat_independent.rs",
      "core/rcdvlog-core/tests/fixtures/v01/**"
    ],
    "integrator": [
      "core/rcdvlog-core/src/lib.rs",
      "docs/tasks/tv-review/EPIC.md",
      "docs/task-cycles/V01-T001/**"
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
    "cargo test -p rcdvlog-core --test v01_t001_compat_independent"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T001\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Add every new type and field from ARCHITECTURE.md §3–§9 as **data only**, with no behaviour change. That covers `SoundtrackPeak`, `DistinctivePolicy`, the new optional fields on report, policy, spec, query, index and `SyncRequest`, `SOUNDTRACK_RECIPE_V2`, `SyncState::ConsensusInterpolated`, and the `ConsensusSegment`/`SoundtrackConsensus` structs in a new `tv_soundtrack_consensus.rs`. Later packets fill in the behaviour. This packet carries the serde-compatibility risk on its own, so later packets cannot break it accidentally.

## Principal invariant

This task is correct when every new v2 type, field, recipe constant and SyncState variant named in ARCHITECTURE.md §3–§9 exists, and every existing v1 spec, plan, index, report and sync request round-trips through serde with byte-identical JSON and unchanged digests.

## Inputs and outputs

Inputs: ARCHITECTURE.md §2–§9; existing DTOs. Outputs: the new types/fields, compiled and exported; v1 golden fixtures under the tester path.

## Frozen decisions

Field names, types and serde attributes exactly as ARCHITECTURE.md §2–§9 specify. Every new field uses `default` plus `skip_serializing_if`. `SyncState::ConsensusInterpolated` serializes as `consensusInterpolated`. Existing schema_version constants stay 1 for objects that set no new field. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: `core/rcdvlog-core/src/tv_soundtrack.rs`, `core/rcdvlog-core/src/tv_soundtrack_tests.rs`, `core/rcdvlog-core/src/tv_soundtrack_session.rs`, `core/rcdvlog-core/src/tv_soundtrack_session_tests.rs`, `core/rcdvlog-core/src/tv_review_sync.rs`, `core/rcdvlog-core/src/tv_review_sync_tests.rs`, `core/rcdvlog-core/src/tv_soundtrack_consensus.rs`, `core/rcdvlog-core/src/tv_soundtrack_consensus_tests.rs`.
- Independent tester: `core/rcdvlog-core/tests/v01_t001_compat_independent.rs`, `core/rcdvlog-core/tests/fixtures/v01/**`.
- Integrator: `core/rcdvlog-core/src/lib.rs`, `docs/tasks/tv-review/EPIC.md`, `docs/task-cycles/V01-T001/**`.
- Forbidden paths always win.

## Required interface

Public structs/fields named in §3–§9; `pub const SOUNDTRACK_RECIPE_V2: &str`; `pub mod tv_soundtrack_consensus;` registered in lib.rs (integrator).

## Acceptance oracle

(1) The bytes of the RC6 plan.json, spec.json and evidence data JSON are unchanged after a parse/serialize round-trip. (2) Every `planDigest` is unchanged. (3) A spec containing `"distinctive":{...}` parses, but setting a range value outside §4 fails `validate()`. (4) `cargo test` passes with no behaviour change: all existing soundtrack tests pass untouched.

## Independent test charter

Claim to falsify: Re-serialise the preserved RC6 run's plan.json, spec.json, target.json and evidence.stdout.json data through the rebuilt types and compare bytes; then set each new field and prove it appears in the output. Copy the real RC6 JSON (`../data/editorial-review/rc6-passive-sync-2026-11-03/{plan,spec,target}.json`, evidence.stdout.json `data`) into fixtures. Assert a byte-identical round-trip and digest equality. Add hand-written v2 JSON with each new field and assert that unknown misspelt fields are still rejected (deny_unknown_fields). Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown, and must never produce an anchor. Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: silent digest drift and accidental behaviour switches. Ask: does any new field serialise when unset? Does any default enable behaviour? Are the validate() ranges exactly those in §4? Severity: any digest drift = blocker. Approval vocabulary: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Any matching, masking, consensus or mapping behaviour.
- CLI changes.

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
cargo test -p rcdvlog-core --test v01_t001_compat_independent
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
