# H01-T002: Review-loop CUJs

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "H01-T002",
  "epic_id": "H01",
  "title": "Review-loop CUJs",
  "slug": "review-loop-cujs",
  "status": "draft",
  "task_class": "implementation",
  "depends_on": ["H01-T001"],
  "principal_invariant": "Every review mutation a user can perform is written through the real adapter, is visible in canonical harness state, and survives navigation, filtering, presentation changes, and a renderer remount.",
  "central_falsification": "Make one mutation write-only — accept it in the UI but do not persist it — and prove a named CUJ fails on the read-back rather than on a spy.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "App review mutation -> canonical state read-back"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/harness/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/harness/ARCHITECTURE.md", "AGENTS.md", "apps/desktop/src/App.tsx"],
    "independent_tester": ["AGENTS.md", "PRD.md", "docs/desktop/smoke-test.md"],
    "reviewer": ["docs/tasks/harness/ARCHITECTURE.md", "AGENTS.md"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/harness/tasks/H01-T002-review-loop-cujs.md"],
    "implementer": ["apps/desktop/src/test/harness/commands/review.ts", "apps/desktop/src/test/harness/state.ts", "apps/desktop/src/test/cuj/review.cuj.test.tsx", "apps/desktop/src/test/cuj/annotations.cuj.test.tsx", "apps/desktop/src/test/cuj/navigation.cuj.test.tsx"],
    "independent_tester": ["apps/desktop/src/__independent__/review-loop.independent.test.tsx"],
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
      "regex": "\\| \\[H01-T002\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Cover the loop the application exists for: open a project, move through clips,
judge them, and come back. CUJ 1–5 and CUJ 12 from the design brief.

## Principal invariant

Notes, labels, removal, rotation, clip summaries, and camera renames each reach
canonical state through the production command, and the UI that reads them back
agrees after navigation, filtering, Focus, and a full remount.

## Inputs and outputs

Inputs: the harness from H01-T001; `list_assets`, `update_asset`,
`create_note_revision`, `add_label`, `remove_label`, `set_removed`,
`set_rotation`, `set_clip_summary`, `rename_camera`, `record_desktop_context`,
`set_autoplay_mode`, `refresh_asset_orientation`.

Outputs: coherent handlers for that family, plus CUJ files for open-and-review,
the judgment loop, Focus, search/filter, rotation, and renderer restart.

## Frozen decisions

- Each mutation CUJ asserts the read-back, not the call: state first, command
  log only where ordering is contractual.
- `create_note_revision` and `add_label`/`remove_label` are distinct commands
  with distinct argument shapes; a CUJ may not accept `update_asset` in their
  place.
- Removal semantics follow `orderAssets`: removed clips leave the active queue
  and appear only under the removed view.
- Rotation persistence is asserted after moving away and back, since that is the
  reported failure mode.
- The remount CUJ tears down and re-renders `App` against the **same** harness
  state; it proves frontend restore semantics, never SQLite durability.

## Owned paths

The implementer owns the review command module, the state mutators it needs, and
the three CUJ files listed. The independent tester owns one file under
`__independent__/`. Neither may edit production source.

## Required interface

```ts
harness.state.assets["asset_sony_001"].notes;
harness.calls("create_note_revision");   // [{ assetId, text }]
harness.remount();                       // unmount + render against same state
```

## Acceptance oracle

1. Opening a project lists clips in `orderAssets` order with the restored clip
   selected.
2. `J`/`K` move the selection, and evidence and annotation panels follow.
3. A note typed and saved appears in `state.assets[id].notes` and is still shown
   after navigating away and back.
4. A label added is in state; removing it removes it from state.
5. Removing a clip drops it from the active queue, shows it under the removed
   view, and restoring returns it to its ordered position.
6. Entering and leaving Focus preserves textarea identity, draft text, and
   selection.
7. Search matches filename, title, transcript text, and label; the
   unusable-only and removed toggles compose with it; clearing restores.
8. Rotation set on a clip survives navigation away and back.
9. After `harness.remount()`, note, label, rotation, selection, and autoplay mode
   are restored from state.

## Independent test charter

Attack coherence between surfaces rather than single writes: label a clip and
check the ribbon, queue row, and evidence shelf agree; remove the selected clip
and check the selection moves somewhere legal; filter to nothing and check the
app stays usable. Use a project with more cameras and clips than the
implementer's fixture.

## Review charter

Risk domain: tests that pass because they assert what they just stubbed. Ask: is
any assertion made only against a spy? Does any CUJ re-implement `orderAssets`
to compute its expectation? Does any mutation test skip the read-back? Severity:
a missing read-back is blocking.

## Non-goals

- Analysis, chronology, generation, or import journeys.
- Layout, scroll, and real-focus assertions that belong to the browser tier.

## Stop conditions

Stop and escalate when a review mutation cannot be observed through any read the
frontend performs, or when a CUJ requires production change to become testable.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
```

## Handoff

Report the CUJs added, the commands newly handled, any production defect the
suite exposed, and the suite's runtime before and after.
