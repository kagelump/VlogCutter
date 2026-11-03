# V01-T011: Docs and planning reconciliation

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "V01-T011",
  "epic_id": "V01",
  "title": "Docs and planning reconciliation",
  "slug": "docs-reconcile",
  "status": "ready",
  "task_class": "integration",
  "depends_on": [
    "V01-T010"
  ],
  "principal_invariant": "tv-soundtrack-matching.md, tv-review-sync.md, tv-review-local-session.md, the epic and PROVIDER-NOTES describe exactly the shipped v2 behaviour and the §10 evidence, with no claim beyond it.",
  "central_falsification": "Reviewer greps the docs for each §3–§9 term and checks each against code; any described behaviour that is absent, or any accuracy claim, rejects.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "docs -> shipped behaviour"
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
      "docs/tasks/tv-review/tasks/V01-T011-docs-reconcile.md"
    ],
    "implementer": [],
    "independent_tester": [],
    "integrator": [
      "docs/tv-soundtrack-matching.md",
      "docs/tv-review-sync.md",
      "docs/tv-review-local-session.md",
      "docs/agent-cli.md",
      "docs/tasks/tv-review/EPIC.md",
      "AGENTS.md",
      "../data/editorial-review/PROVIDER-NOTES.md",
      "docs/tasks/tv-review/EPIC.md",
      "docs/task-cycles/V01-T011/**"
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
    "cargo fmt --all -- --check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/tv-review/EPIC.md",
      "regex": "\\| \\[V01-T011\\].*\\| complete \\|"
    },
    {
      "path": "../data/editorial-review/PROVIDER-NOTES.md",
      "regex": "rc6-passive-sync-v2"
    }
  ]
}
-->

## Goal

Update the canonical docs and close the epic.

## Principal invariant

This task is correct when tv-soundtrack-matching.md, tv-review-sync.md, tv-review-local-session.md, the epic and PROVIDER-NOTES describe exactly the shipped v2 behaviour and the §10 evidence, with no claim beyond it.

## Inputs and outputs

Inputs: merged code, the T010 report. Outputs: updated docs and the epic row complete.

## Frozen decisions

Wording must keep the acceptance-boundary caveat: no independent timing ground truth. Also apply all of ARCHITECTURE.md §2.

## Owned paths

- Contract editor: this packet only.
- Implementer: none (no production changes).
- Independent tester: none.
- Integrator: `docs/tv-soundtrack-matching.md`, `docs/tv-review-sync.md`, `docs/tv-review-local-session.md`, `docs/agent-cli.md`, `docs/tasks/tv-review/EPIC.md`, `AGENTS.md`, `../data/editorial-review/PROVIDER-NOTES.md`, `docs/tasks/tv-review/EPIC.md`, `docs/task-cycles/V01-T011/**`.
- Forbidden paths always win.

## Required interface

None.

## Acceptance oracle

Every §3–§9 public field and CLI flag is documented; the PROVIDER-NOTES RC6 section links the v2 report; AGENTS.md §3 mentions consensus and v2 in one line.

## Independent test charter

Claim to falsify: Reviewer greps the docs for each §3–§9 term and checks each against code; any described behaviour that is absent, or any accuracy claim, rejects. Not applicable: documentation only. The reviewer performs the grep check. Required failure behaviour: wrong or ambiguous evidence must stay unmatched, ambiguous or unknown, and must never produce an anchor. Preservation: existing soundtrack, session and sync tests stay green. Evidence: command output and test names.

## Review charter

Risk: overclaiming. Approval: APPROVE / REQUEST_CHANGES / BLOCK.

## Non-goals

- Code changes.

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
```

## Handoff

List changed paths, every command run with its complete result, compatibility effects (v1 digests unchanged?), known limitations, and any stop condition hit. The implementer does not approve the task.
