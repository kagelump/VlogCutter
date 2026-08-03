# D01-T025: Documentation and planning reconciliation

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T025",
  "epic_id": "D01",
  "title": "Documentation and planning reconciliation",
  "slug": "documentation-and-reconciliation",
  "status": "blocked",
  "task_class": "integration",
  "depends_on": ["D01-T018", "D01-T024", "D01-T029", "D01-T030"],
  "principal_invariant": "Every canonical document describes what the repository actually does, with no capability claimed that no packet delivered and no delivered capability left undocumented.",
  "central_falsification": "For each capability in the PRD section 21.2 launch-critical list, locate both the code that implements it and the acceptance evidence for it; any claim without both, or any shipped capability absent from the documents, falsifies the reconciliation.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": ["shipped behavior -> canonical documentation"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/EPIC.md"],
    "independent_tester": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/EPIC.md"],
    "reviewer": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["PRD.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T025-documentation-and-reconciliation.md"],
    "implementer": [],
    "independent_tester": ["docs/task-cycles/D01-T025/audit/**"],
    "integrator": [
      "PRD.md",
      "AGENTS.md",
      "README.md",
      "docs/desktop/chronology.md",
      "docs/desktop/edit-plan-and-export.md",
      "docs/tasks/desktop/EPIC.md",
      "docs/agent-cli.md",
      "docs/task-cycles/D01-T025/evidence/**"
    ]
  },
  "forbidden_paths": ["core/**", "apps/**", "packages/**", "scripts/**", ".github/workflows/**"],
  "verification_commands": [
    "pnpm check:docs",
    "cargo test --workspace",
    "pnpm check:frontend",
    "pnpm test:desktop"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T025\\].*\\| complete \\|"
    },
    {
      "path": "PRD.md",
      "regex": "## Current implementation checkpoint — 2026"
    },
    {
      "path": "AGENTS.md",
      "regex": "chronology"
    }
  ]
}
-->

## Current implementation checkpoint

The completion review corrected the top-level implementation checkpoints in
`AGENTS.md`, `README.md`, `PRD.md`, `docs/agent-cli.md`, and
`docs/integration-testing.md` so they no longer describe the new domain code as
absent. Final user documentation, acceptance verdicts, packet reconciliation,
and epic closure remain blocked on every dependency above.

## Goal

Close the epic by making the documents match the repository, so the next person to read
`PRD.md` or `AGENTS.md` is told the truth about what exists.

The `PRD.md` checkpoint currently says the desktop "does not yet implement ingest
preview/capture-sequence editing, trip-context persistence, expanded chronology
calibration, edit planning, preview render, timeline export". Most of that changes in this
epic, and a stale checkpoint is how the imbalance this epic corrects came about in the
first place.

## Principal invariant

This task is correct when every canonical document describes what the repository actually
does, with no capability claimed that no packet delivered and no delivered capability left
undocumented.

## Inputs and outputs

Inputs:

- Every completed packet's handoff report in this epic.
- `docs/tasks/desktop/BASELINE_AUDIT.md`, `SMOKE_LEDGER.md`,
  `ACCEPTANCE_CHRONOLOGY.md`, and `ACCEPTANCE_RESOLVE.md` — the acceptance evidence.
- `PRD.md` §21.2, the launch-critical capability list, as the checklist.

Outputs:

- `PRD.md` implementation checkpoint rewritten for the new state, with the same honesty
  the current one shows about what did not run.
- `AGENTS.md` §3 (implementation versus PRD target) and §12 (testing table, including the
  CI workflow and `pnpm test:desktop`) updated.
- `README.md` updated where it describes desktop capabilities.
- `docs/desktop/chronology.md` — the user-facing chronology workflow.
- `docs/desktop/edit-plan-and-export.md` — the handoff procedure, already begun by
  D01-T024.
- `docs/agent-cli.md` — the chronology CLI surface, already begun by D01-T017.
- The epic's queue rows reconciled to `complete`.

## Frozen decisions

- **Claim only what has evidence.** A capability implemented but never exercised by an
  acceptance packet is described as implemented and unverified, not as working. The
  current `PRD.md` checkpoint models this well and the standard must not drop.
- **Name what did not happen.** Any packet that ended `blocked`, any defect deferred, and
  any acceptance row that did not run gets an explicit statement. Silence reads as
  success.
- **No production code changes.** If a document cannot be made true without changing code,
  the code is wrong and that is a separate packet.
- **Retain the outstanding server-epic risks.** The Ubuntu/Compose, NAS, LAN, and
  Tailscale rows from `docs/tasks/server/ACCEPTANCE_TASK_19.md` remain outstanding and
  must not be quietly dropped while adjacent text is rewritten.
- `pnpm check:docs` must pass, since the repository already has a docs check and a
  documentation packet that breaks it is self-defeating.

## Owned paths

Integration class: the integrator owns every canonical planning and documentation file,
per the rule that planning reconciliation belongs to the integrator. All source paths are
forbidden. The independent tester owns a separate audit directory.

## Required interface

The `PRD.md` checkpoint keeps its existing shape — a dated heading followed by prose
stating what is implemented, what is not, and what acceptance did not run — so a reader
familiar with the current document finds the same structure.

`AGENTS.md` §12's table gains rows for the desktop frontend suite and the CI workflow:

```md
| React/TypeScript/CSS | `pnpm --dir apps/desktop check` | `pnpm test:desktop` plus interaction/visual check |
| Every change | — | `.github/workflows/ci.yml` on push and pull request |
```

## Acceptance oracle

1. Every capability in `PRD.md` §21.2 is classified in the checkpoint as implemented and
   verified, implemented and unverified, or not implemented — with no capability
   unclassified.
2. Every "implemented and verified" claim names the acceptance document that verifies it.
3. Every packet in the epic queue whose work is done shows `complete`, and every packet's
   own `reconciliation_checks` regexes match.
4. Any packet still `blocked` or `draft` is named in the checkpoint with why.
5. `AGENTS.md` §3 no longer lists as missing anything this epic delivered.
6. `AGENTS.md` §12 references the CI workflow and the desktop test command.
7. `docs/desktop/chronology.md` describes the workflow completely enough for the owner to
   repeat the Antarctica correction from it: sequences, epochs, exceptions, phases,
   placement, contradictions, and how to read an explanation.
8. `docs/desktop/edit-plan-and-export.md` describes the handoff including any conversion
   step D01-T024 found.
9. The outstanding server-epic acceptance risks are still stated.
10. `pnpm check:docs` passes.
11. No file under `core/`, `apps/`, `packages/`, or `scripts/` is modified.
12. No document claims a capability whose acceptance packet was rejected.

## Independent test charter

Falsify the documents against the repository.

- Take `PRD.md` §21.2 line by line. For each capability, find the code that implements it
  and the acceptance document that exercised it. Record capability, code location,
  evidence location. Report every row missing either.
- Work the other direction too: list the Tauri commands, CLI subcommands, and migrations
  that exist, and find each one in the documents. An undocumented capability is as much a
  defect as an overclaimed one, and is the direction reviewers usually skip.
- Grep the documents for confident language — "works", "supports", "verified",
  "complete" — and check each instance against evidence.
- Confirm every epic row marked `complete` has a handoff report and a review approval, and
  that every packet's reconciliation regexes actually match by running the epic validator.
- Confirm the server epic's outstanding risks still appear, by diffing that section
  against its pre-change text.
- Confirm no source file changed, with `git diff --stat`.
- Read `docs/desktop/chronology.md` as someone who has not used the app, and attempt the
  Antarctica correction from it alone. A step you cannot follow is a finding.

## Review charter

Risk domain: documentation drift in the optimistic direction, which is how a project comes
to believe it is further along than it is — the exact condition this epic was created to
correct.

Questions:

1. Does any document claim a capability without acceptance evidence?
2. Is any delivered capability undocumented?
3. Are blocked, draft, and deferred items named explicitly?
4. Were the server epic's outstanding risks preserved?
5. Does the checkpoint keep the honesty of the version it replaces, or has hedging been
   dropped in favour of a cleaner narrative?
6. Was any source file modified?
7. Is the chronology document sufficient for someone to repeat the workflow?

Severity rules: an unevidenced claim, an undocumented capability, a dropped outstanding
risk, or a modified source file is blocking. Wording, ordering, and heading style are
non-blocking.

Smallest acceptable remedy: restate the claim at the accuracy the evidence supports, or add
the missing entry.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Changing any production code.
- Rewriting `PRD.md` beyond the checkpoint and any section this epic made false.
- Documenting the companion server or travel epics beyond preserving their status.
- Marketing copy or release notes.
- Promoting the epic, which is the human's decision.

## Stop conditions

Stop and escalate when:

- a document cannot be made true without a code change;
- a capability has no acceptance evidence and no packet was scheduled to produce it,
  meaning the epic has a coverage gap rather than a documentation gap;
- an acceptance packet ended `rejected` and it is unclear whether the epic can be promoted
  at all;
- `pnpm check:docs` fails for a reason requiring a script change, which is outside this
  packet's paths;
- the Antarctica gate is still `blocked`, in which case say so plainly in the checkpoint
  rather than describing chronology as validated.

## Commands

```sh
pnpm check:docs
cargo test --workspace
pnpm check:frontend
pnpm test:desktop
python3 .claude/skills/writing-bounded-task-packets/scripts/epic_queue.py docs/tasks/desktop/EPIC.md --validate
git diff --stat -- core apps packages scripts
```

Expected result: the checks pass, the epic validates, and the final `git diff --stat`
reports no changes.

## Handoff

Report the capability-by-capability classification table with code and evidence locations,
the documents updated, the blocked and deferred items named, confirmation that the server
epic's outstanding risks were preserved, the epic validator output, and the empty source
diff. State plainly whether the epic is promotable and, if not, what remains. List any stop
condition encountered. The integrator does not approve this task, and does not promote the
epic — that is the human's decision.

## Promotion note

`draft` until D01-T018 and D01-T024 reach a verdict, since this packet's honesty depends on
having those verdicts to report. The contract editor promotes it once both acceptance
documents exist, whatever they say.
