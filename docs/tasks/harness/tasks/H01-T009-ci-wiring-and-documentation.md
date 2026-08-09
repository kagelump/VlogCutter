# H01-T009: CI wiring and documentation reconciliation

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "H01-T009",
  "epic_id": "H01",
  "title": "CI wiring and documentation reconciliation",
  "slug": "ci-wiring-and-documentation",
  "status": "complete",
  "task_class": "integration",
  "depends_on": ["H01-T008"],
  "principal_invariant": "The repository's testing authorities describe the tier that actually exists — what it runs, what it gates, and what it may never be cited as evidence for — and the CI workflow encodes exactly that.",
  "central_falsification": "Read the updated AGENTS.md and integration-testing.md as an outsider and try to conclude that the fast suite proves native, media, or domain behavior; the wording must make that reading impossible.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "gate table -> CI workflow steps",
    "tier definition -> what each suite may claim"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/harness/ARCHITECTURE.md", "docs/tasks/harness/EPIC.md"],
    "implementer": ["AGENTS.md", "docs/integration-testing.md", ".github/workflows/ci.yml", "package.json"],
    "independent_tester": ["AGENTS.md", "docs/integration-testing.md"],
    "reviewer": ["AGENTS.md", "docs/integration-testing.md", "PRD.md"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/harness/tasks/H01-T009-ci-wiring-and-documentation.md"],
    "implementer": ["package.json", "apps/desktop/package.json", "apps/desktop/vitest.config.ts", ".github/workflows/ci.yml", "AGENTS.md", "docs/integration-testing.md", "docs/desktop/smoke-test.md"],
    "independent_tester": [],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "forbidden_paths": ["core/**", "apps/desktop/src-tauri/**", "apps/web/**", "apps/server/**", "apps/cli/**"],
  "verification_commands": [
    "pnpm --dir apps/desktop test",
    "pnpm test:desktop:cuj",
    "pnpm check:docs"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/harness/EPIC.md",
      "regex": "\\| \\[H01-T009\\].*\\| complete \\|"
    },
    {
      "path": "AGENTS.md",
      "regex": "test:desktop:cuj"
    },
    {
      "path": "docs/integration-testing.md",
      "regex": "[Ff]ast desktop CUJ"
    }
  ]
}
-->

## Goal

Make the new tier a first-class, named gate, and reconcile every document that
tells a reader or an agent what the desktop's tests prove.

## Principal invariant

`AGENTS.md` §12, `docs/integration-testing.md`, `.github/workflows/ci.yml`, and
the package scripts agree with each other and with the suite as built, including
an explicit statement of what the fast tier may never be cited for.

## Inputs and outputs

Inputs: the completed suite from H01-T001 through H01-T008.

Outputs: a `test:desktop:cuj` script and its workspace-level alias, a named CI
step, a new row in the integration-testing coverage table, a gate-table row in
`AGENTS.md` §12, and a smoke-test document that no longer covers ground the fast
tier now covers automatically.

## Frozen decisions

- The CUJ tier runs inside the existing `pnpm test:desktop` gate; the separate
  script exists for iteration and for a named CI step, not to create a second
  gate that can be forgotten.
- `AGENTS.md`'s rule stands: a new gate command in the table gets a matching
  named workflow step, and the two never drift.
- The "do not pretend headless tests validate" list in `AGENTS.md` §12 gains the
  CUJ tier by name, not just by implication.
- `docs/integration-testing.md` gains one row. No existing row is relabelled, and
  no manual acceptance item is marked covered.
- The desktop smoke-test document keeps every step that needs a human or a Mac,
  and drops only steps the CUJ suite now proves in CI — each removal names the
  test that replaced it.

## Owned paths

The implementer owns the four documents, the two package files, the vitest
config, and the workflow. No test files change in this task.

## Required interface

```sh
pnpm test:desktop         # unit + component + CUJ
pnpm test:desktop:cuj     # the CUJ tier alone
```

## Acceptance oracle

1. `pnpm test:desktop:cuj` runs only CUJ files and passes.
2. `pnpm test:desktop` still runs everything and passes.
3. `.github/workflows/ci.yml` has a named step for the new script with the
   `if: ${{ !cancelled() }}` guard every other gate carries.
4. `AGENTS.md` §12's table lists the fast CUJ suite with its trigger and its
   broader check, and its exclusion list names the tier.
5. `docs/integration-testing.md` has a fast-CUJ row that states the boundary and
   does not claim native, media, or domain evidence.
6. `pnpm check:docs` passes.
7. Every smoke-test step removed cites the CUJ that replaced it.

## Independent test charter

Read only the changed documents and try to answer: what does a green
`pnpm test:desktop` entitle me to claim about media playback, about the
chronology solver, about a real card import? Every answer must be "nothing".
Report any sentence that permits a different reading.

## Review charter

Risk domain: documentation that quietly promotes simulated evidence. Ask: does
any changed sentence let a green fast suite stand in for a native, media, or
topology gate? Does the workflow match the table exactly? Severity: any
promotion of simulated evidence is blocking.

## Non-goals

- Adding a browser or native suite. Scoping them is part of this task's writing;
  building them is separate work.
- Changing Rust or web gates.

## Stop conditions

Stop and escalate when the CUJ tier cannot be separated cleanly by the test
runner's configuration, or when reconciling a document would require weakening an
existing gate.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm test:desktop:cuj
pnpm check:docs
```

## Handoff

Report the scripts and workflow steps added, every document line changed, and
the final tier counts: fast CUJ scenarios, browser scenarios, native scenarios.
