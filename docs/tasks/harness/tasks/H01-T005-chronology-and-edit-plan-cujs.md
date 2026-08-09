# H01-T005: Chronology and edit-plan CUJs

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "H01-T005",
  "epic_id": "H01",
  "title": "Chronology and edit-plan CUJs",
  "slug": "chronology-and-edit-plan-cujs",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["H01-T001"],
  "principal_invariant": "The calibration and edit-plan surfaces are exercised against scenario-declared backend outcomes, with preview always preceding a write and queue position always captured before a mutation, and no chronology or edit-plan rule implemented in TypeScript.",
  "central_falsification": "Declare a preview that returns a hard contradiction and prove the UI issues no mutating command at all, not merely that it shows a warning.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "chronologyApi -> scenario-declared outcome table"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/harness/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/EDIT_PLAN_MODEL.md", "docs/tasks/desktop/DIAGNOSTICS_MODEL.md"],
    "independent_tester": ["docs/tasks/desktop/ACCEPTANCE_CHRONOLOGY.md", "docs/integration-testing.md"],
    "reviewer": ["docs/tasks/harness/ARCHITECTURE.md", "docs/tasks/desktop/CHRONOLOGY_MODEL.md"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/harness/tasks/H01-T005-chronology-and-edit-plan-cujs.md"],
    "implementer": ["apps/desktop/src/test/harness/commands/chronology.ts", "apps/desktop/src/test/harness/commands/editPlan.ts", "apps/desktop/src/test/cuj/chronology.cuj.test.tsx", "apps/desktop/src/test/cuj/editPlan.cuj.test.tsx", "apps/desktop/src/test/harness/scenarios/chronology.ts"],
    "independent_tester": ["apps/desktop/src/__independent__/chronology-safety.independent.test.tsx"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "forbidden_paths": ["core/**", "apps/desktop/src-tauri/**", "apps/web/**", "apps/server/**", "apps/cli/**"],
  "verification_commands": [
    "pnpm --dir apps/desktop test",
    "pnpm --dir apps/desktop check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/harness/EPIC.md",
      "regex": "\\| \\[H01-T005\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Bring the calibration, diagnostics, and edit-plan surfaces into the fast tier
without building a second chronology solver. CUJ 10 and 11 from the design
brief, plus the edit-plan lifecycle.

## Principal invariant

Chronology and edit-plan responses are declared per scenario as backend
outcomes; the CUJs prove the frontend's safety contract — preview before write,
capture before mutate, refresh after, resume where the user was — and prove
nothing about whether the declared outcome is mathematically right.

## Inputs and outputs

Inputs: harness from H01-T001; the full `chronologyApi` surface: overview,
explain, epoch preview/set/clear, sequence split/merge, clip time exceptions,
trip phases, event-order constraints, queue capture/restore, diagnostics,
edit-plan revisions, validation, preview render, OTIO export.

Outputs: chronology and edit-plan command modules driven by a declarative
outcome table, a chronology scenario, and two CUJ files.

## Frozen decisions

- A scenario declares the overview returned before an edit and, keyed by named
  edit, the overview returned after it. No handler computes an ordering.
- `preview_clock_epoch` returns scenario-declared contradictions. When any is
  hard, the CUJ asserts **no** mutating command was issued — an assertion on the
  command log, which is the case the log exists for.
- Diagnostics, validation reports, and OTIO results are likewise declared.
- Edit-plan revisions carry an expected-active-revision id; a stale expectation
  is a scenario-declared conflict, and the CUJ proves the UI surfaces it without
  writing.
- The chronology CUJs assert queue restoration by asset id, not by index.

## Owned paths

The implementer owns the two command modules, the scenario, and the two CUJ
files. The independent tester owns one file under `__independent__/`.

## Required interface

```ts
desktopScenario()
  .chronology({ order: ["asset_sony_001", "asset_dji_001"] })
  .whenClockEpochApplied("seq_sony_1", { offsetSeconds: 3600 })
  .thenChronology({ order: ["asset_dji_001", "asset_sony_001"] })
  .contradictionOnPreview("seq_dji_1", { strength: "hard", summary: "..." });
```

## Acceptance oracle

1. Entering chronology mode loads the declared overview and renders its
   sequences, epochs, and phases.
2. Editing a clock epoch previews first: the command log shows
   `preview_clock_epoch` before `set_clock_epoch`, and the impact is shown to the
   user before confirmation.
3. Confirming writes, refreshes the overview to the declared post-edit ordering,
   and returns the user to review on the same clip they were on.
4. `capture_queue_position` is issued before the mutation and
   `restore_queue_position` after it.
5. A preview returning a hard contradiction disables Apply, shows the
   contradiction, and issues no mutating command.
6. Diagnostics render, and acknowledging one removes it from the default view.
7. The edit-plan panel creates a revision, validates it, and blocks export on a
   blocking finding.
8. A stale `expectedActiveRevisionId` surfaces a conflict and performs no write.
9. OTIO export requires the declared digest expectation and reports the declared
   result.

## Independent test charter

Attack the safety contract from unusual entry points: start an edit, navigate
away, and come back; apply two edits in sequence; confirm a preview whose
scenario outcome changed underneath. Assert command ordering and absence of
writes, never chronological correctness.

## Review charter

Risk domain: a TypeScript solver sneaking in. Ask: does any handler derive an
order, a contradiction, or a validation finding rather than look one up? Does
any test assert that a computed chronology is correct? Severity: any derived
chronology rule is blocking.

## Non-goals

- Solver correctness, which belongs to `cargo test -p rcdvlog-core`.
- Preview rendering or OTIO file contents.
- Resolve import.

## Stop conditions

Stop and escalate when a UI flow cannot be exercised without deriving a
chronology rule, or when the declared-outcome table cannot express a needed
transition.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
```

## Handoff

Report the outcome-table shape, the flows covered, the safety orderings
asserted, and every chronology claim explicitly left to Rust.
