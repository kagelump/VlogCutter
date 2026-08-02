# D01-T015: Expanded calibration surface, read-only

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T015",
  "epic_id": "D01",
  "title": "Expanded calibration surface, read-only",
  "slug": "calibration-surface-read-only",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["D01-T003", "D01-T014"],
  "principal_invariant": "The calibration view renders the chronology solution faithfully, showing every asset in solver order on its own capture-sequence lane under the trip phase that governs its display time.",
  "central_falsification": "Render a project whose solution is known independently and compare every lane assignment, block position, and displayed time against the solver output; any divergence falsifies the view.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["chronology solution -> calibration render"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "AGENTS.md"],
    "independent_tester": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "AGENTS.md"],
    "reviewer": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md", "PRD.md", "AGENTS.md"],
    "integrator": ["docs/tasks/desktop/CHRONOLOGY_MODEL.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T015-calibration-surface-read-only.md"],
    "implementer": [
      "apps/desktop/src/calibration/CalibrationView.tsx",
      "apps/desktop/src/calibration/lanes.ts",
      "apps/desktop/src/calibration/lanes.test.ts",
      "apps/desktop/src/calibration/CalibrationView.test.tsx",
      "apps/desktop/src/App.tsx",
      "apps/desktop/src/api.ts",
      "apps/desktop/src/types.ts",
      "apps/desktop/src/styles.css",
      "apps/desktop/src-tauri/src/lib.rs"
    ],
    "independent_tester": ["apps/desktop/src/__independent__/calibration-view.independent.test.tsx"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T015/**"]
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
      "regex": "\\| \\[D01-T015\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Build the expanded calibration view from `PRD.md` §3.2 as a read-only surface:
trip-context band, time ruler, camera and card lanes, filename blocks, raw versus
best-local timestamps, and a confidence and evidence panel.

Read-only on purpose. A reviewer can reject the visualization without rejecting the
editing interactions, and the editing packet that follows starts from a surface
already proven to display the truth.

## Principal invariant

This task is correct when the calibration view renders the chronology solution
faithfully, showing every asset in solver order on its own capture-sequence lane
under the trip phase that governs its display time.

## Inputs and outputs

Inputs:

- The solution, explanations, and contradictions from D01-T014, exposed through new
  read-only Tauri commands.
- The extracted review modules and vitest harness from D01-T003.

Outputs:

- New read-only Tauri commands and their DTO mirrors.
- `lanes.ts` — pure layout math: lane assignment, block extents, ruler ticks.
- `CalibrationView.tsx` — the rendering surface.
- An entry point from `App.tsx` to open and close the view.

## Frozen decisions

- Read-only. No control in this view emits an operation. The apply path arrives in
  D01-T016.
- Layout math lives in `lanes.ts` as pure functions with no React and no DOM, so it
  is unit-testable, following the D01-T003 pattern.
- `api.ts` remains the only module calling `invoke`, per `AGENTS.md` §10.
- One lane per capture sequence, not per camera. Two cards from one camera are two
  lanes; that distinction is the model's whole point and must be visible.
- Both times are always shown for every asset: the raw camera timestamp and the
  derived best local time. A view that shows only the corrected time hides what was
  corrected.
- Confidence is shown as a value with its evidence list, never as a bare colour.
  Colour may reinforce it but must not be the only encoding.
- The view opens as an expanded overlay over the review screen and returns to the
  review screen with the same selection, per `PRD.md` §3.2.
- Keyboard accessible: opening, closing, moving selection, and reaching the evidence
  panel all work without a mouse, per `AGENTS.md` §10.

## Owned paths

The implementer owns the calibration modules, the additive `App.tsx` entry point,
the `api.ts` and `types.ts` additions, styles, and the new read-only Tauri
commands. The core chronology modules are forbidden: if the view needs data the
core does not expose, that is a stop condition, not a licence to reach into the
domain.

## Required interface

```rust
// apps/desktop/src-tauri/src/lib.rs, read-only commands
#[tauri::command]
fn project_chronology(project_id: String, state: State<'_, AppState>)
    -> Result<ChronologyView, String>;

#[tauri::command]
fn explain_asset_chronology(asset_id: String, state: State<'_, AppState>)
    -> Result<ChronologyExplanation, String>;
```

```ts
// apps/desktop/src/types.ts, mirrors
export interface ChronologyLane {
  captureSequenceId: string;
  label: string;
  cameraKey: string;
  volumeHint: string;
}

export interface ChronologyBlock {
  assetId: string;
  captureSequenceId: string;
  filename: string;
  filenameCounter: number | null;
  rawTime: string | null;
  tripTime: string | null;
  displayPrimary: string | null;
  displaySecondary: string | null;
  timezone: string | null;
  phaseId: string | null;
  confidence: number;
  uncertaintySeconds: number;
}

export interface ChronologyView {
  lanes: ChronologyLane[];
  blocks: ChronologyBlock[];
  phases: TripPhaseView[];
  contradictions: ContradictionView[];
}
```

```ts
// apps/desktop/src/calibration/lanes.ts — pure
export interface RulerTick { atMillis: number; label: string; major: boolean; }

export function assignLanes(view: ChronologyView): ChronologyLane[];
export function blockExtent(block: ChronologyBlock, pixelsPerMinute: number):
  { left: number; width: number };
export function rulerTicks(fromMillis: number, toMillis: number, pixelsPerMinute: number):
  RulerTick[];
export function phaseExtent(phase: TripPhaseView, pixelsPerMinute: number):
  { left: number; width: number };
```

## Acceptance oracle

1. A project with two cameras and three capture sequences renders three lanes, with
   each asset on the lane of its own sequence.
2. Two sequences reusing counters `C0001`–`C0050` render as two distinct lanes with
   no block appearing twice.
3. Every block shows both its raw timestamp and its derived local time. An asset
   with a null raw timestamp shows an explicit empty state, not a blank.
4. The trip band renders one region per phase, positioned to cover the same span
   the solver used, with transit phases visually distinguished from location
   phases.
5. An asset covered by no phase renders in a visibly uncovered region and displays
   UTC.
6. A dual-mode transit phase shows both primary and secondary times for its blocks.
7. Selecting a block shows its confidence, uncertainty, evidence list, and applied
   corrections from `explain_asset_chronology`.
8. A project with a hard contradiction shows it, naming the assets involved.
9. Block order within a lane matches solver order exactly.
10. `rulerTicks` produces ticks at sensible intervals for spans of one hour, one
    day, one week, and one month, without producing thousands of ticks at any zoom.
11. Opening and closing the view preserves the review selection.
12. Every interactive element is reachable and operable by keyboard, and each has an
    accessible name.
13. No command in this view mutates anything: after opening, browsing every block,
    and closing, the catalog file hash is unchanged.

## Independent test charter

Falsify render fidelity against the solver.

- Build your own project fixture and compute the expected lane assignment, block
  order, and displayed times independently from the solver's own output — read the
  solution through the CLI or core surface, not through the view's own code path.
- Assert every block's lane, order, and both times against that independent
  expectation. Report the first divergence.
- Attack the layout math directly in `lanes.ts`: a zero-duration span; a span of
  ten years; `pixelsPerMinute` of 0.001 and of 1000; a block whose trip time is
  null; two blocks at the identical millisecond; 5,000 blocks in one lane.
- Attack the phase band: overlapping phases; a zero-width phase; a phase entirely
  outside the asset time range; a project with no phases at all.
- Verify keyboard reachability by traversing the view with keyboard events only and
  asserting the evidence panel is reachable.
- Verify immutability by hashing the catalog before and after a full browse.
- Confirm `invoke` appears only in `api.ts` by grepping the calibration directory.

## Review charter

Risk domain: a view that misrepresents the solution, which is worse than no view —
the user would make corrections based on a false picture.

Questions:

1. Does any block render on a lane other than its own capture sequence's?
2. Is block order within a lane the solver's order, or a re-sort in the view?
3. Is the raw timestamp always shown alongside the derived time?
4. Is confidence encoded by anything other than colour alone?
5. Does any control in this read-only view emit an operation?
6. Is `lanes.ts` genuinely pure, with no React import and no DOM access?
7. Does `invoke` appear anywhere outside `api.ts`?
8. Is the whole surface keyboard operable with accessible names?

Severity rules: a wrong lane, a re-sorted order, a hidden raw timestamp, a mutation
from a read-only view, or a keyboard-unreachable control is blocking. Visual
density, colour choices, and tick spacing aesthetics are non-blocking.

Smallest acceptable remedy: correct the mapping and add the fixture case that
exposed it.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Any editing: offset controls, drag, before/after placement, impact preview,
  apply. All of that is D01-T016.
- Clock-anchor confirmation, deferred by `PRD.md` §21.3.
- The compact five-source overview in the review screen, which already exists.
- Changing the core chronology modules.
- Virtualized rendering optimizations beyond what the acceptance budget requires.

## Stop conditions

Stop and escalate when:

- the view needs solution data the core does not expose, which requires a core
  change this packet does not own;
- rendering 5,000 blocks is not responsive without a rendering architecture
  decision such as virtualization;
- lane assignment is ambiguous for an asset whose sequence is missing;
- the overlay cannot preserve review selection without restructuring `App.tsx`
  state beyond an additive entry point;
- displaying dual times requires a layout decision the product owner has not made.

## Commands

```sh
pnpm --dir apps/desktop check
pnpm --dir apps/desktop test
cargo test -p rcdvlog-desktop
pnpm --dir apps/desktop build
grep -rn "invoke" apps/desktop/src/calibration/
```

Expected result: checks and tests pass, the build succeeds, and the grep finds no
`invoke` call inside the calibration directory.

## Handoff

Report the commands added, the DTO mirrors, the layout functions with their tested
edge cases, the render-fidelity comparison against the independent expectation, the
keyboard traversal result, and the immutability evidence. Include a screenshot of
the view rendering a multi-sequence multi-phase project. List any stop condition
encountered. The implementer does not approve this task.

## Promotion note

`draft` until D01-T014 completes and `docs/tasks/desktop/CHRONOLOGY_MODEL.md` is
published. The contract editor confirms the DTO mirrors match the frozen Rust types
before promoting.
