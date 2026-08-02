# D01-T016: Calibration editing interactions and impact preview

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T016",
  "epic_id": "D01",
  "title": "Calibration editing interactions and impact preview",
  "slug": "calibration-editing-interactions",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["D01-T015"],
  "principal_invariant": "No chronology change reaches the catalog without an explicit user apply, and the impact preview shown before apply matches exactly what apply produces.",
  "central_falsification": "Drag an offset across a range, record the previewed impact, apply, and diff the actual derived-time changes against the preview; any difference, and any catalog change observed before apply, falsifies the interaction.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["calibration interaction -> chronology operations"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "AGENTS.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "AGENTS.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "AGENTS.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T016-calibration-editing-interactions.md"],
    "implementer": [
      "apps/desktop/src/calibration/impact.ts",
      "apps/desktop/src/calibration/impact.test.ts",
      "apps/desktop/src/calibration/CalibrationEditor.tsx",
      "apps/desktop/src/calibration/CalibrationEditor.test.tsx",
      "apps/desktop/src/calibration/CalibrationView.tsx",
      "apps/desktop/src/api.ts",
      "apps/desktop/src/types.ts",
      "apps/desktop/src/styles.css",
      "apps/desktop/src-tauri/src/lib.rs"
    ],
    "independent_tester": ["apps/desktop/src/__independent__/calibration-apply.independent.test.tsx"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T016/**"]
  },
  "forbidden_paths": ["core/rcdvlog-core/src/chronology/**", "core/rcdvlog-server/**", "apps/web/**", "apps/server/**", "apps/cli/**", ".github/**"],
  "verification_commands": [
    "pnpm --dir apps/desktop check",
    "pnpm --dir apps/desktop test",
    "cargo test -p rcdvlog-desktop",
    "pnpm --dir apps/desktop build"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T016\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Make the calibration surface editable: choose an adjustment scope, set an offset
numerically or by dragging, place a clip before or after another, see the impact,
and explicitly apply or cancel.

This is the packet that turns chronology from a data model into the workflow the
product owner asked for.

## Principal invariant

This task is correct when no chronology change reaches the catalog without an
explicit user apply, and the impact preview shown before apply matches exactly what
apply produces.

## Inputs and outputs

Inputs:

- The read-only calibration surface from D01-T015.
- `preview_clock_epoch` from D01-T010 and `preview_contradictions` from D01-T013 —
  the core-side preview functions that guarantee preview and apply share a path.
- `capture_queue_position` and `restore_queue_position` from D01-T014.

Outputs:

- New mutating Tauri commands wrapping the core chronology operations.
- `impact.ts` — pure functions turning a preview into a displayable diff.
- `CalibrationEditor.tsx` — scope selection, offset control, drag, before/after
  placement, impact panel, apply and cancel.

## Frozen decisions

- Preview is computed by the core, never by the UI. `PRD.md` §21.4 forbids
  automatic chronology changes, and a UI-computed preview would be a second
  implementation of the solver that can disagree with it. `impact.ts` formats a
  preview; it never computes one.
- Explicit apply and cancel, always. There is no auto-apply, no apply-on-blur, and
  no debounce that commits. Cancel restores the pre-edit state with no operation
  emitted.
- Adjustment scope is chosen from what the model supports: a whole capture
  sequence, a counter range within one sequence, or a single clip. There is no
  free-form multi-select, because the model cannot express a correction over an
  arbitrary asset set and pretending otherwise would produce a control whose result
  the user cannot predict.
- Drag adjusts the same offset value the numeric control edits. Both write one
  candidate, so they cannot diverge.
- Before/after placement emits an `eventOrderConstraintSet` operation. It is not a
  manual sort and it does not write a time.
- A hard contradiction blocks apply, and the blocking report names the conflicting
  evidence in the UI, per D01-T013.
- Queue position is captured before apply and restored after, per D01-T014.
- Every interaction is keyboard operable. Drag has a keyboard equivalent — the
  numeric control with arrow keys — so calibration is never mouse-only.

## Owned paths

The implementer owns the editor, the impact formatting module, the mutating Tauri
commands, and the additive `CalibrationView.tsx` changes to host the editor. The
core chronology modules are forbidden; needing to change them is a stop condition.

## Required interface

```rust
// apps/desktop/src-tauri/src/lib.rs
#[tauri::command]
fn preview_chronology_change(project_id: String, candidate: CandidateChange, state: State<'_, AppState>)
    -> Result<ChronologyImpact, String>;

#[tauri::command]
fn apply_chronology_change(project_id: String, candidate: CandidateChange, state: State<'_, AppState>)
    -> Result<AppliedChronologyChange, String>;
```

```ts
// apps/desktop/src/types.ts
export type AdjustmentScope =
  | { kind: "sequence"; captureSequenceId: string }
  | { kind: "range"; captureSequenceId: string; startCounter: number; endCounter: number | null }
  | { kind: "clip"; assetId: string };

export type CandidateChange =
  | { kind: "offset"; scope: AdjustmentScope; offsetSeconds: number; label: string }
  | { kind: "exception"; assetId: string; tripTime: string; reason: string }
  | { kind: "placement"; earlierAssetId: string; laterAssetId: string; strength: "hard" | "soft" }
  | { kind: "phase"; phase: TripPhaseView };

export interface ChronologyImpact {
  changes: { assetId: string; before: string | null; after: string | null }[];
  contradictions: ContradictionView[];
  blocked: boolean;
}

export interface AppliedChronologyChange {
  resumeAssetId: string;
  changedAssetCount: number;
}
```

```ts
// apps/desktop/src/calibration/impact.ts — pure, formats only
export interface ImpactSummary {
  movedCount: number;
  unchangedCount: number;
  largestShiftSeconds: number;
  earliestAffected: string | null;
  latestAffected: string | null;
}
export function summarizeImpact(impact: ChronologyImpact): ImpactSummary;
export function impactRows(impact: ChronologyImpact):
  { assetId: string; before: string; after: string; shiftSeconds: number }[];
```

## Acceptance oracle

1. Dragging an offset over a range shows an impact listing exactly the assets in
   that range, and the catalog file hash is unchanged until apply.
2. Applying produces derived-time changes identical to the previewed impact — same
   asset ids, same before and after values.
3. Cancel after a drag leaves the catalog unchanged and emits no operation.
4. The numeric control and drag produce the same candidate for the same offset
   value.
5. Scope `sequence` affects the whole sequence; scope `range` affects only its
   counters; scope `clip` affects one asset.
6. Before/after placement emits a constraint operation and writes no time.
7. A placement that would create a hard contradiction sets `blocked` true, apply is
   refused, and the UI names the conflicting evidence.
8. After an apply that reorders the queue, the review screen resumes on
   `resumeAssetId`.
9. Applying with a stale revision surfaces the conflict to the user rather than
   overwriting.
10. Every control is reachable and operable by keyboard, including a keyboard path
    to adjust the offset without dragging, and each has an accessible name.
11. An impact affecting 2,000 assets renders without freezing the UI, and the
    summary reports the count rather than listing all rows unbounded.
12. No control auto-applies: a grep of the editor finds no operation call outside
    the apply handler.

## Independent test charter

Falsify the "nothing is written before apply" and "preview equals apply" claims.

- Hash the catalog file before opening the editor, after every interaction short of
  apply, and after cancel. Any change before apply is a blocking finding.
- For each candidate kind — offset over a sequence, offset over a range, exception,
  placement, phase — capture the preview, apply, then read derived times through a
  path other than the UI, such as the CLI, and diff against the preview.
- Attack the boundary cases: an offset of zero; an offset of ±14 hours; a range of a
  single counter; a range with no assets; a placement between an asset and itself; a
  placement duplicating an existing constraint.
- Attack cancel: cancel mid-drag; cancel after switching scope; cancel after a
  blocked apply attempt. Assert no operation in `project_operations` for each.
- Attack the blocked path: create a hard constraint, then attempt an offset that
  violates it, and confirm apply is refused with nothing written.
- Drive the entire flow by keyboard only, including offset adjustment and apply, and
  confirm it is completable.
- Confirm `impact.ts` computes no times of its own by grepping it for date
  arithmetic; it must only format values the core supplied.

## Review charter

Risk domain: a preview that lies, or a change applied without the user asking —
either one destroys trust in a feature whose entire purpose is careful correction.

Questions:

1. Can any interaction write to the catalog before apply?
2. Does `impact.ts` compute any time itself rather than formatting core output?
3. Do drag and the numeric control share one candidate representation?
4. Does cancel guarantee no operation was emitted?
5. Is a hard contradiction genuinely blocking in the UI, with the evidence named?
6. Is queue position captured before apply and restored after?
7. Is every interaction keyboard operable, including offset adjustment?
8. Is scope limited to what the model can express?

Severity rules: a write before apply, a preview that diverges from apply, a
non-blocking hard contradiction, a lost queue position, or a mouse-only interaction
is blocking. Drag sensitivity, impact-table pagination, and visual polish are
non-blocking.

Smallest acceptable remedy: route the computation through the core preview and add
the hash-comparison test that exposed the early write.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Clock-anchor confirmation UI, deferred by `PRD.md` §21.3.
- Editing trip-phase boundaries by dragging the band, beyond setting a phase through
  the `phase` candidate — band dragging is a `PRD.md` §10.3 nicety and is not
  required for the MVP loop.
- Bulk multi-select correction across arbitrary assets, which the model cannot
  express.
- Undo history beyond cancel-before-apply.
- Changing the core chronology modules or the solver.

## Stop conditions

Stop and escalate when:

- preview and apply cannot be made to share a core path, meaning the UI would have
  to compute times;
- an adjustment the user clearly wants cannot be expressed by any supported scope;
- blocking a hard contradiction in the UI requires a core change;
- restoring queue position after apply requires restructuring `App.tsx` state
  beyond additive changes;
- a 2,000-asset impact cannot be rendered responsively without a rendering
  architecture decision.

## Commands

```sh
pnpm --dir apps/desktop check
pnpm --dir apps/desktop test
cargo test -p rcdvlog-desktop
pnpm --dir apps/desktop build
grep -rn "apply_chronology_change" apps/desktop/src/
```

Expected result: checks, tests, and build pass, and the grep shows the apply command
called from exactly one handler.

## Handoff

Report the candidate kinds implemented, the preview-equals-apply diff results for
each, the catalog-hash evidence that nothing is written before apply, the blocked
contradiction behavior, the queue resumption result, and the keyboard-only walkthrough
result. Include a screenshot of the impact panel before an apply. List any stop
condition encountered. The implementer does not approve this task.

## Promotion note

`draft` until D01-T015 completes. The contract editor confirms that
`preview_clock_epoch` and `preview_contradictions` are available and share the apply
path before promoting, since the principal invariant depends on it.
