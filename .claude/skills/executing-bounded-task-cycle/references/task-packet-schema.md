# Task packet schema v1

Every task packet is Markdown containing one machine-readable JSON metadata
comment and the required human-readable sections.

## Metadata block

Place this immediately after the title:

```md
<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "E01-T001",
  "epic_id": "E01",
  "title": "Route the production scan through PreviewDecoder",
  "slug": "decoder-seam",
  "status": "ready",
  "task_class": "implementation",
  "depends_on": [],
  "principal_invariant": "Every production scan decode passes through the decoder owned by ProjectService.",
  "central_falsification": "Inject a marked decoder through ProjectService, run scan, and inspect the generated preview.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": ["ProjectService -> imaging scanner"],
  "documents_by_role": {
    "contract_editor": ["planning/ARCHITECTURE_CONTRACT.md"],
    "implementer": ["planning/PRODUCT_CONTRACT.md", "planning/ARCHITECTURE_CONTRACT.md"],
    "independent_tester": ["planning/PRODUCT_CONTRACT.md", "planning/ARCHITECTURE_CONTRACT.md"],
    "reviewer": ["planning/PRODUCT_CONTRACT.md", "planning/ARCHITECTURE_CONTRACT.md"],
    "integrator": ["planning/ARCHITECTURE_CONTRACT.md"]
  },
  "path_ownership": {
    "contract_editor": ["planning/epics/E01/tasks/E01-T001-decoder-seam.md"],
    "implementer": ["crates/core/src/imaging/decoder.rs", "crates/core/src/imaging/scanner.rs", "crates/core/tests/decoder_seam.rs"],
    "independent_tester": ["crates/core/tests/decoder_seam_independent.rs"],
    "integrator": ["crates/core/src/imaging/mod.rs", "docs/task-cycles/E01-T001/**"]
  },
  "forbidden_paths": ["apps/**", "crates/core/src/catalog/**"],
  "verification_commands": ["cargo test -p core", "cargo clippy --workspace -- -D warnings"],
  "reconciliation_checks": [
    {
      "path": "planning/epics/E01/EPIC.md",
      "regex": "\\| \\[E01-T001\\].*\\| complete \\|"
    }
  ]
}
-->
```

## Allowed statuses

- `draft`
- `ready`
- `in_progress`
- `code_complete`
- `integrated`
- `independently_tested`
- `reviewed`
- `gated`
- `complete`
- `blocked`
- `design_required`

The epic row and task metadata must agree. Detailed transient state belongs in
the cycle evidence manifest, not in prose claims.

## Task classes

- `foundation`: senior-owned architecture or shared infrastructure decision. It may be `ready` with `architecture_decisions_frozen: false` because freezing that decision is the task; its packet must state decision authority, alternatives, criteria, and downstream interface output.
- `implementation`: bounded leaf implementation suitable for a focused agent.
- `integration`: shared hotspot, manifest, migration, or CI wiring.
- `qa`: independent test-only work.
- `review`: read-only or evidence-only risk review.

## Admission requirements for implementation tasks

An implementation packet is admitted only when all are true:

1. Its correctness can be summarized by one principal invariant.
2. Its central claim can be falsified independently.
3. Architecture decisions are already frozen.
4. It crosses no more than one load-bearing seam.
5. Role path ownership is exhaustive and non-overlapping.
6. A reviewer could reject this task without rejecting adjacent tasks.
7. It fits one focused implementation context.
8. Stop conditions identify when the agent must escalate instead of redesigning.

Failure actions:

| Failure | Required action |
| --- | --- |
| More than one invariant | Split the task |
| Multiple unrelated seams | Split at a stable interface |
| Open architecture choice | Create or complete a foundation task first |
| No independent falsification | Return to design; do not invent an oracle |
| Paths cannot be exhaustively owned | Investigate or split |
| Too large for one context | Split while preserving one end-to-end test cycle |

## Required Markdown sections

Every packet contains these exact second-level headings:

- `## Goal`
- `## Principal invariant`
- `## Inputs and outputs`
- `## Frozen decisions`
- `## Owned paths`
- `## Required interface`
- `## Acceptance oracle`
- `## Independent test charter`
- `## Review charter`
- `## Non-goals`
- `## Stop conditions`
- `## Commands`
- `## Handoff`

No required section may contain `TBD`, `TODO`, or `???` when status is `ready`.

## Ownership rules

- Each role receives only its list in `documents_by_role` plus the selected task packet and necessary source files.
- The implementer may change only `path_ownership.implementer`.
- The independent tester may change only `path_ownership.independent_tester`.
- The contract editor may change only `path_ownership.contract_editor`.
- The integrator may change the union of all role paths.
- A forbidden path always wins over an allowed path.
- Shared hot spots belong to the integrator, not the leaf implementer.
- An unexpected dependency, migration, manifest, or architecture path triggers a stop.

## Reconciliation checks

`reconciliation_checks` are regular expressions that must match after the task
is complete. Use them for the epic row, foundation ledger, roadmap, or other
canonical planning locations. They prevent a report from declaring completion
while the repository still says the task is open.
