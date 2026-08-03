# D01-T029: Desktop chronology and edit-plan regression suite

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T029",
  "epic_id": "D01",
  "title": "Desktop chronology and edit-plan regression suite",
  "slug": "desktop-chronology-edit-plan-regression-suite",
  "status": "blocked",
  "task_class": "integration",
  "depends_on": ["D01-T015", "D01-T016", "D01-T023"],
  "principal_invariant": "The desktop's completed chronology and edit-plan workflows are protected by component tests that exercise the same public client boundary and user controls the application uses.",
  "central_falsification": "Break one queue-resume, validation-blocking, overwrite-confirmation, or keyboard interaction at a time and prove a focused component test fails for each break.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": ["desktop client boundary -> React workflow", "React workflow -> keyboard and accessibility contract"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md", "AGENTS.md"],
    "implementer": ["AGENTS.md", "docs/tasks/desktop/CHRONOLOGY_MODEL.md", "docs/tasks/desktop/EDIT_PLAN_MODEL.md"],
    "independent_tester": ["AGENTS.md", "PRD.md"],
    "reviewer": ["AGENTS.md", "PRD.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["AGENTS.md", "docs/tasks/desktop/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T029-desktop-chronology-edit-plan-regression-suite.md"],
    "implementer": [
      "apps/desktop/src/chronologyApi.ts",
      "apps/desktop/src/calibration/**/*.test.ts*",
      "apps/desktop/src/editplan/**/*.test.ts*",
      "apps/desktop/src/test/**"
    ],
    "independent_tester": ["apps/desktop/src/__independent__/desktop-mvp-workflows.independent.test.tsx"],
    "integrator": ["docs/tasks/desktop/EPIC.md", "docs/task-cycles/D01-T029/**"]
  },
  "forbidden_paths": ["core/**", "apps/desktop/src-tauri/**", "apps/web/**", "apps/server/**", "apps/cli/**", ".github/**"],
  "verification_commands": [
    "pnpm --dir apps/desktop test",
    "pnpm --dir apps/desktop check",
    "pnpm --dir apps/desktop build",
    "pnpm check:frontend"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T029\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Add the missing automated desktop regression layer after the chronology and
edit-plan panels reach their accepted shape. The branch added more than 700
lines of workflow UI while the desktop suite still tests only review ordering
and key dispatch.

## Principal invariant

The completed workflows are protected through the same exported client and
accessible controls the application uses; tests must not reimplement chronology
or edit-plan rules in React.

## Inputs and outputs

Inputs are the completed D01-T015, D01-T016, and D01-T023 components and the
Vitest/testing-library harness. Outputs are an injectable chronology/edit-plan
client seam, focused component tests, and one independent end-to-end component
test covering queue resume through export confirmation.

## Frozen decisions

- The core remains the authority; test doubles return canonical DTOs.
- Tests may prove React behavior, not Tauri persistence, native dialogs, media
  playback, or FFmpeg execution.
- User interactions use accessible roles/names and `user-event`.
- Each mutation test asserts preview/confirmation precedes the client write.
- No snapshot-only suite: tests assert state and calls users depend on.

## Owned paths

The implementer owns the client injection seam and focused component tests. The
independent tester owns one separate workflow test. Production core, Tauri, CLI,
and web code are forbidden.

## Required interface

Export a default production client plus a narrow provider or prop-based override
that tests can replace without modifying `window.__TAURI_INTERNALS__`. Components
continue to consume one typed client interface.

## Acceptance oracle

1. Read-only chronology rendering covers lanes, phase bands, uncovered/dual
   display, selection explanation, contradictions, and diagnostics.
2. Every chronology mutation proves preview, explicit confirmation, write,
   parent-cache refresh, and queue resume ordering.
3. Edit-plan tests cover seed, reorder, trim, remove, validation blocking,
   staleness warning, preview progress/playback, export history, and overwrite
   confirmation.
4. Keyboard-only operation reaches every control and preserves visible focus.
5. Rejected/stale calls keep prior UI state and expose an alert.
6. Mutating one behavior named in `central_falsification` makes its test fail.
7. The suite does not claim native or media acceptance.

## Independent test charter

Construct canonical DTO fixtures independently. Drive one chronology change
that reorders the queue and one edit-plan export that first refuses an existing
file, then succeeds after digest confirmation. Assert call order and accessible
status, not component internals.

## Review charter

Reject tests that call component functions directly, assert only snapshots, use
invented DTO fields, or make the browser mock look like native acceptance.
Confirm production uses the same typed interface as tests.

## Non-goals

- Native Tauri automation.
- FFmpeg/ffprobe execution.
- Core solver, validation, or OTIO tests.
- Visual redesign.

## Stop conditions

Stop if completing the suite requires moving a domain rule into TypeScript, if
the final components still lack stable accessible controls, or if a proposed
test can pass while its user-visible behavior is broken.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
pnpm --dir apps/desktop build
pnpm check:frontend
```

## Handoff

List workflows covered, test counts, the deliberate mutations used to prove the
oracles, and native/media behaviors that remain manual.
