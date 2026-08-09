# H01-T007: Import and card CUJs

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "H01-T007",
  "epic_id": "H01",
  "title": "Import and card CUJs",
  "slug": "import-and-card-cujs",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["H01-T003"],
  "principal_invariant": "The card-import journey — detect, preflight, choose a destination, run, and review the result — is exercised through the production commands, and a completed import adds exactly the clips its preflight classified as new.",
  "central_falsification": "Run an import whose preflight classified three clips as new and prove the project gains exactly those three, with the known clips neither duplicated nor re-copied.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "import plan -> canonical asset state"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/harness/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/import/ARCHITECTURE.md", "docs/tasks/import/EPIC.md"],
    "independent_tester": ["docs/tasks/import/EPIC.md", "docs/integration-testing.md"],
    "reviewer": ["docs/tasks/harness/ARCHITECTURE.md", "docs/tasks/import/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/harness/tasks/H01-T007-import-and-card-cujs.md"],
    "implementer": ["apps/desktop/src/test/harness/commands/import.ts", "apps/desktop/src/test/cuj/import.cuj.test.tsx", "apps/desktop/src/test/harness/scenarios/cardInserted.ts"],
    "independent_tester": ["apps/desktop/src/__independent__/import-journey.independent.test.tsx"],
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
      "regex": "\\| \\[H01-T007\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Cover the import journey the I01 epic just shipped, at the level between its
component tests and its physical card acceptance: the whole flow, through the
real commands, against coherent state.

## Principal invariant

An import run adds exactly the assets its preflight classified as new, leaves
known clips untouched, and reports copied bytes consistent with the plan it was
given — all observable in canonical state after the run.

## Inputs and outputs

Inputs: harness from H01-T001 and the generation handlers from H01-T003;
`detect_mounted_cards`, `inspect_import_source`, `list_archive_roots`,
`create_archive_root`, `bind_archive_root`, `set_import_preferences`,
`preflight_import`, `run_smart_import`, `cancel_import`, the `import-progress`
event, `ingest_folder`, and `relink_folder`.

Outputs: an import command module whose run applies its plan to state, a
card-inserted scenario, and the import CUJ file.

## Frozen decisions

- Preflight is pure: the handler must not mutate state, and the CUJ asserts that.
- `run_smart_import` is a scripted job; progress steps mark items done and the
  completion applies exactly the plan's new items to `state.assets`.
- Destination blocking (insufficient space, unavailable root) is scenario state,
  not a UI-only condition.
- A second import of the same card with additional clips adds only the additions
  — the incremental case the import epic cares most about.
- Nothing here is evidence for real copying, verification hashing, or card
  identity. `docs/tasks/import/EPIC.md`'s physical gate is unchanged.

## Owned paths

The implementer owns the import command module, the scenario, and the CUJ file.
The independent tester owns one file under `__independent__/`.

## Required interface

```ts
desktopScenario()
  .cardInserted("DJI_SD", { clips: 8, newClips: 3, cameraHint: "DJI Pocket 3" })
  .archiveRoot("NAS Video", { availableBytes: 1_200_000_000_000 });

const job = harness.job("run_smart_import");
await job.progress({ stage: "copying", completed: 2, total: 3 });
await job.complete();
```

## Acceptance oracle

1. An inserted card shows the banner, and opening import shows the card, its
   clip count, and the resolved destination.
2. Preflight leaves canonical state unchanged.
3. A destination without room blocks the run and says why.
4. Running the import renders progress from emitted events and, on completion,
   adds exactly the new clips to the project.
5. The completion summary's counts match the state diff.
6. Re-importing the same card after adding clips imports only the additions.
7. Cancelling mid-run keeps the items already imported and imports no more.
8. The post-import "review last import" path selects an imported clip.

## Independent test charter

Attack the accounting with awkward plans: a card of entirely known clips, a card
with a path collision, a card with unreadable items, and an archive root that
goes unavailable between preflight and run. Assert the state diff each time.

## Review charter

Risk domain: an import fake that is kinder than a real card. Ask: does preflight
mutate? Does the run add items its plan did not classify as new? Does any test
claim evidence for copy verification? Severity: a state diff that disagrees with
the reported outcome is blocking.

## Non-goals

- Real filesystem copying, hashing, or removable-volume detection.
- Archive relocation and rebinding, which is a physical gate in I01.

## Stop conditions

Stop and escalate when the import UI cannot be driven without a native dialog
that the harness cannot script, or when a plan cannot be applied to state
without duplicating ingest rules.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
```

## Handoff

Report the flows covered, the classifications exercised, and the boundary
between what this proves and what the I01 physical gate still owes.
