# D01-T023: Edit-plan and export desktop UI

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T023",
  "epic_id": "D01",
  "title": "Edit-plan and export desktop UI",
  "slug": "edit-plan-and-export-ui",
  "status": "blocked",
  "task_class": "implementation",
  "depends_on": ["D01-T021", "D01-T022"],
  "principal_invariant": "A user can build a plan from their included clips, see its validation findings and staleness, render a preview, and export a timeline, without any invalid plan reaching render or export.",
  "central_falsification": "Drive the whole surface with a plan containing an offline original and a stale script revision, and confirm the error blocks render and export while the warning does not, and that no output file is produced for the blocked case.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": ["edit plan core surface -> desktop edit plan UI"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md", "AGENTS.md"],
    "independent_tester": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md", "AGENTS.md"],
    "reviewer": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md", "PRD.md", "AGENTS.md"],
    "integrator": ["docs/tasks/desktop/EDIT_PLAN_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T023-edit-plan-and-export-ui.md"],
    "implementer": [
      "apps/desktop/src/editplan/EditPlanView.tsx",
      "apps/desktop/src/editplan/EditPlanView.test.tsx",
      "apps/desktop/src/editplan/plan.ts",
      "apps/desktop/src/editplan/plan.test.ts",
      "apps/desktop/src/App.tsx",
      "apps/desktop/src/api.ts",
      "apps/desktop/src/types.ts",
      "apps/desktop/src/styles.css",
      "apps/desktop/src-tauri/src/lib.rs"
    ],
    "independent_tester": ["apps/desktop/src/__independent__/edit-plan.independent.test.tsx"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T023/**"]
  },
  "forbidden_paths": ["core/rcdvlog-core/src/**", "core/rcdvlog-server/**", "apps/web/**", "apps/server/**", "apps/cli/**", ".github/**"],
  "verification_commands": [
    "pnpm --dir apps/desktop check",
    "pnpm --dir apps/desktop test",
    "cargo test -p rcdvlog-desktop",
    "pnpm --dir apps/desktop build"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T023\\].*\\| complete \\|"
    }
  ]
}
-->

## Current implementation checkpoint

`EditPlanPanel.tsx` can seed or rebuild a full-range plan from non-removed clips,
show validation findings, start a preview, display progress text, and request an
OTIO export. It cannot reorder, trim, remove, or save an edited operation list;
show script staleness; play the preview; show revisions or export history; use a
native save dialog; or confirm a digest-guarded overwrite. It also has no pure
plan helper or component tests. Resume after D01-T021 and D01-T022 are complete.

## Goal

Give the user the last screen the MVP loop needs: build an edit plan from the clips they
included, set source in and out ranges, see validation findings and staleness, render a
preview, export a timeline, and review export history.

## Principal invariant

This task is correct when a user can build a plan from their included clips, see its
validation findings and staleness, render a preview, and export a timeline, without any
invalid plan reaching render or export.

## Inputs and outputs

Inputs:

- The plan model, validation, preview, and OTIO export from D01-T019 through D01-T022.
- Chronology order from D01-T014 as the plan's default ordering.
- The vitest harness and extracted-module pattern from D01-T003.

Outputs:

- New Tauri commands wrapping the plan, validation, preview, and export surfaces.
- `plan.ts` — pure functions building a candidate plan from included clips and
  reordering operations.
- `EditPlanView.tsx` — the plan surface with preview and export controls and history.

## Frozen decisions

- The default plan is the included, non-removed clips in chronology order, with each
  clip's full duration as its source range. The user then trims and reorders. Building a
  plan by hand from scratch would make the feature unusable on a real trip.
- Source in and out are edited numerically. No scrubbing timeline widget: `PRD.md` §21.4
  defers general-purpose timeline editing, and a drag-trim widget is the first step onto
  that path.
- Validation findings are always visible before render and export. Errors block both
  actions in the UI; warnings are shown and do not block, matching the severities
  D01-T020 froze.
- Staleness against the script revision is visible, per `PRD.md` §21.1 step 11, and does
  not block.
- Export refuses to silently replace a newer output. The UI surfaces the refusal and
  requires an explicit confirmation that names what is being overwritten.
- `api.ts` remains the only module calling `invoke`.
- Reordering and plan construction live in `plan.ts` as pure functions, testable without
  a DOM.
- Preview progress, worker count, and estimated time remaining are shown using the
  existing batch progress conventions rather than a new pattern.
- Every control is keyboard operable with an accessible name.

## Owned paths

The implementer owns the edit-plan UI modules, the additive `App.tsx` entry point, the
`api.ts` and `types.ts` additions, styles, and the new Tauri commands. All of
`core/rcdvlog-core/src/**` is forbidden: the UI consumes the core surface and must not
reimplement validation, ordering, or export.

## Required interface

```rust
// apps/desktop/src-tauri/src/lib.rs
#[tauri::command]
fn edit_plan(project_id: String, state: State<'_, AppState>) -> Result<EditPlanRevisionView, String>;

#[tauri::command]
fn default_edit_plan(project_id: String, state: State<'_, AppState>) -> Result<Vec<EditPlanOperationView>, String>;

#[tauri::command]
fn save_edit_plan(project_id: String, operations: Vec<EditPlanOperationView>, expected_active_revision_id: Option<String>, state: State<'_, AppState>) -> Result<EditPlanRevisionView, String>;

#[tauri::command]
fn validate_edit_plan_command(revision_id: String, state: State<'_, AppState>) -> Result<ValidationReportView, String>;

#[tauri::command]
async fn render_edit_plan_preview(revision_id: String, state: State<'_, AppState>) -> Result<PreviewResultView, String>;

#[tauri::command]
fn export_edit_plan_otio(revision_id: String, output_path: String, expected_existing_digest: Option<String>, state: State<'_, AppState>) -> Result<OtioExportResultView, String>;

#[tauri::command]
fn edit_plan_export_history(project_id: String, state: State<'_, AppState>) -> Result<Vec<EditPlanExportView>, String>;
```

```ts
// apps/desktop/src/editplan/plan.ts — pure
export function defaultPlanFromAssets(assets: Asset[]): EditPlanOperationView[];
export function reorder(ops: EditPlanOperationView[], from: number, to: number): EditPlanOperationView[];
export function setSourceRange(ops: EditPlanOperationView[], position: number, inSeconds: number, outSeconds: number): EditPlanOperationView[];
export function removeOperation(ops: EditPlanOperationView[], position: number): EditPlanOperationView[];
export function totalDurationSeconds(ops: EditPlanOperationView[]): number;
export function hasBlockingFindings(report: ValidationReportView): boolean;
```

`reorder`, `setSourceRange`, and `removeOperation` all return renumbered positions
starting at 0, so the invariant that positions are contiguous holds in the UI as well as
in the database.

## Acceptance oracle

1. Opening the view on a project with no plan offers the default plan: included,
   non-removed clips in chronology order with full source ranges.
2. Saving creates a revision whose operations match what the UI showed, in order.
3. Reordering an operation renumbers positions contiguously from 0, with no duplicate
   and no gap.
4. Editing source in and out updates the displayed total duration.
5. Removing an operation renumbers the rest contiguously.
6. Validation findings display with their severity. An error disables both the render and
   export controls; a warning does not.
7. A plan with an offline original cannot be rendered or exported from the UI, and no
   output file is produced.
8. A stale script revision shows a visible staleness indicator and blocks nothing.
9. Rendering shows progress, a worker count, and an estimated time remaining, and the
   completed preview is playable in the app.
10. Export writes a file and adds an entry to the visible export history.
11. Exporting to a path that already exists surfaces the refusal and requires an explicit
    confirmation naming what is being overwritten before proceeding.
12. Saving with a stale expected revision surfaces the conflict, naming the revision that
    superseded it, and does not overwrite.
13. Every control is reachable and operable by keyboard, with an accessible name.
14. `plan.ts` functions are pure: no React import, no `invoke`, no DOM access.

## Independent test charter

Falsify the claim that an invalid plan cannot reach render or export.

- Build a plan with each error-severity finding in turn and attempt render and export
  through the UI path. Assert no output file appears on disk and no FFmpeg process runs
  for any of them.
- Attack the block itself: attempt to trigger render and export by keyboard activation
  while the control is disabled, and by invoking the handler directly if the component
  exposes it. A control that is visually disabled but still callable is not a block.
- Attack position renumbering in `plan.ts` with property tests: for any sequence of
  reorder, remove, and setSourceRange operations, positions remain contiguous from 0 with
  no duplicates.
- Attack `setSourceRange` with inverted, zero-length, negative, and NaN values, and
  confirm the UI surfaces the resulting validation finding rather than crashing.
- Attack the overwrite path: export, then export again to the same path, and confirm the
  confirmation names the existing output and that declining leaves the original file
  byte-identical.
- Attack the revision guard by saving two revisions from one base through two paths and
  confirming the second is refused with the superseding revision named.
- Drive the entire flow — build, trim, reorder, validate, render, export — by keyboard
  only.
- Confirm `invoke` appears only in `api.ts` by grepping the editplan directory.

## Review charter

Risk domain: an invalid plan reaching render or export, and a UI that reimplements core
logic and so can disagree with it.

Questions:

1. Can render or export be triggered while an error-severity finding exists?
2. Does the UI compute validation, ordering, duration semantics, or export content
   itself rather than consuming the core?
3. Are positions always contiguous from 0 after any editing operation?
4. Is the overwrite confirmation explicit and does it name what is being replaced?
5. Is a stale-revision save refused with the superseding revision identified?
6. Does `invoke` appear outside `api.ts`?
7. Is any scrubbing or drag-trim timeline widget introduced, contrary to the frozen
   decision?
8. Is the whole surface keyboard operable?

Severity rules: render or export of an invalid plan, a silent overwrite, a silent
revision replacement, core logic reimplemented in the UI, or a keyboard-unreachable
control is blocking. Layout, density, and control placement are non-blocking.

Smallest acceptable remedy: gate the action on `hasBlockingFindings` at the handler
rather than only in the rendered state, and add the direct-invocation test that exposed
the gap.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- A scrubbing timeline, drag-trim, transitions, effects, colour, or audio mixing.
- Multi-track editing.
- Editing the script, which is separate `PRD.md` §23 Milestone 5 work.
- Resolve import verification, which is D01-T024.
- Changing any core module.
- Export formats other than OTIO.

## Stop conditions

Stop and escalate when:

- the default plan cannot be built because chronology order is unavailable for some
  assets;
- preview playback in the app requires a media capability WebKit does not provide,
  which is the `PRD.md` §20 WebKit risk materializing;
- the overwrite confirmation cannot be made race-free between the digest check and the
  write;
- blocking render and export on findings requires a core change;
- a control the user needs cannot be expressed without a timeline widget, which would
  breach the accepted scope and needs the product owner's decision.

## Commands

```sh
pnpm --dir apps/desktop check
pnpm --dir apps/desktop test
cargo test -p rcdvlog-desktop
pnpm --dir apps/desktop build
grep -rn "invoke" apps/desktop/src/editplan/
```

Expected result: checks, tests, and build pass, and the grep finds no `invoke` call
inside the editplan directory.

## Handoff

Report the commands added, the DTO mirrors, the pure plan functions with their property
test results, the blocking behavior for every error finding with evidence that no output
was produced, the overwrite confirmation behavior, the revision conflict behavior, and
the keyboard-only walkthrough result. Include screenshots of the plan surface, a
validation report with a blocked export, and the export history. List any stop condition
encountered. The implementer does not approve this task.

## Readiness note

Blocked on D01-T021 and D01-T022. The initial panel is retained as the starting
point; complete the editor and export UX once its media/export contracts are gated.
