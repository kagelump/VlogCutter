# H01-T010: Browser-preview rationalization

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "H01-T010",
  "epic_id": "H01",
  "title": "Browser-preview rationalization",
  "slug": "browser-preview-rationalization",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["H01-T009"],
  "principal_invariant": "The repository holds one desktop fake-backend state machine, reached by two front doors — the Vite preview and the CUJ harness — or a recorded decision explaining why two are worth their maintenance cost.",
  "central_falsification": "Change one behavior in the shared state model and prove both the preview and the CUJ suite observe the change, with no second implementation left that could disagree.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "api.ts preview branches -> shared scenario state"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/harness/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/harness/ARCHITECTURE.md", "apps/desktop/src/api.ts", "AGENTS.md"],
    "independent_tester": ["docs/integration-testing.md", "docs/desktop/smoke-test.md"],
    "reviewer": ["docs/tasks/harness/ARCHITECTURE.md", "AGENTS.md"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/harness/tasks/H01-T010-browser-preview-rationalization.md"],
    "implementer": ["apps/desktop/src/api.ts", "apps/desktop/src/chronologyApi.ts", "apps/desktop/src/test/harness/**", "apps/desktop/src/preview/**"],
    "independent_tester": ["apps/desktop/src/__independent__/preview-parity.independent.test.ts"],
    "integrator": ["docs/tasks/harness/EPIC.md", "docs/tasks/harness/ARCHITECTURE.md"]
  },
  "forbidden_paths": ["core/**", "apps/desktop/src-tauri/**", "apps/web/**", "apps/server/**", "apps/cli/**"],
  "verification_commands": [
    "pnpm --dir apps/desktop test",
    "pnpm --dir apps/desktop check",
    "pnpm --dir apps/desktop build"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/harness/EPIC.md",
      "regex": "\\| \\[H01-T010\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Decide, with the harness proven, whether the Vite preview should run on the same
fake state machine — and if so, do it, so the repository stops maintaining two
fakes that can disagree.

## Principal invariant

Either the preview and the CUJ harness share one state model and one set of
behaviors, or the file records why they should not, with the specific preview
concerns that make sharing worse.

## Inputs and outputs

Inputs: `api.ts`'s `!inTauri()` branches; `chronologyApi.ts`'s empty-result
branches; the proven `DesktopHarnessState` and its scenarios.

Outputs: either a preview built on the shared scenario state — installed at
startup outside Tauri, so `api.ts`'s adapters take one path in every environment
— or a written decision to keep them separate.

## Frozen decisions

- Correctness first: the preview may not weaken the harness. If serving demo
  concerns would force non-determinism, ambient state, or timing into the test
  harness, they stay separate and the decision is recorded.
- The preview keeps behaving like a preview: it may seed richer demo data and may
  animate progress on real timers, as long as that lives in preview-only code.
- `chronologyApi`'s refusal to invent chronology in the preview stands. A shared
  model may serve declared outcomes, never computed ones.
- If unified, the preview's fake is installed through the same
  `window.__TAURI_INTERNALS__` seam, and the `!inTauri()` branches shrink or
  disappear rather than being duplicated.
- The removal of a preview branch requires a CUJ or component test covering the
  behavior it provided.

## Owned paths

The implementer owns the adapters, the harness, and any new preview module. The
independent tester owns one parity file.

## Required interface

If unified:

```ts
// main.tsx, outside Tauri only
if (!("__TAURI_INTERNALS__" in window)) installPreviewBackend();
```

## Acceptance oracle

1. `pnpm --dir apps/desktop build` succeeds and the preview at port 1420 still
   opens a project, plays through its states, and exercises progress and cancel.
2. No behavior exists in two implementations: a change in the shared model is
   observable from both doors.
3. The CUJ suite is unchanged in strictness — unknown commands still throw, no
   handler reports work it did not do, no harness timestamp is ambient.
4. The preview's demo data is deterministic, or its non-determinism is confined
   to preview-only code the harness never loads.
5. If the decision is to keep two fakes, this file records the reasons and
   `ARCHITECTURE.md` §11 is updated to match.

## Independent test charter

Look for divergence rather than for bugs: pick five behaviors the preview
provides — proxy counts, analysis progress, settings persistence, card
detection, pairing — and check each is served by exactly one implementation.

## Review charter

Risk domain: a refactor that makes the test harness serve demo concerns.
Ask: has any determinism guarantee been relaxed? Does the harness now import
preview data? Can the preview's convenience code reach a CUJ? Severity: any
relaxation of harness determinism is blocking.

## Non-goals

- Improving the preview's demo content.
- Changing production behavior inside Tauri.

## Stop conditions

Stop and escalate when unification would require the harness to accept ambient
state, real timers, or non-deterministic identity.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
pnpm --dir apps/desktop build
```

## Analysis recorded 2026-08-09 (T001–T009 complete, not yet executed)

Unification is the right end state and is **not blocked by anything
architectural**. It is blocked on one dependency edge and one thing this branch
could not verify, both stated here so whoever picks it up starts from facts
rather than from a survey.

### What already works in its favour

Installing the harness outside Tauri would flip `inTauri()` to true, so every
`!inTauri()` branch in `api.ts` becomes unreachable the moment the preview
installs it. The branches would not need to be *replaced* — they would need to
be *deleted*, which is a mechanical diff against dead code rather than a
behavior change. `chronologyApi.ts`'s empty-result branches go the same way, and
the preview would gain the calibration and edit-plan surfaces it has never had,
served by declared outcomes rather than by an invented solver.

### The one edge that has to be cut first

`DesktopHarness` imports `act` from `@testing-library/react` to wrap
harness-initiated React updates (events, job steps, media events). A preview
that imports the harness would drag a devDependency toward the shipped bundle.

The fix is small and known: make the act wrapper injectable — a constructor
option defaulting to a passthrough, with `renderApp` supplying the act version —
and load the preview backend through a dynamic `import()` behind the
`!inTauri()` check so it lands in its own chunk and never in the desktop app.
Roughly twenty lines in `DesktopHarness.ts` and `renderApp.tsx`, plus a new
`src/preview/` module.

### Why it was not done on this branch

The acceptance oracle requires the Vite preview at port 1420 to be opened and
exercised — progress, cancel, playback states, the import modal. That is a
visual check on a running app, and this branch had no way to perform it. Landing
a production refactor of the preview path without ever looking at the preview
would be the same mistake the epic exists to correct: shipping a change whose
evidence is "the tests still pass" when the tests were never the thing at risk.

### Cost of waiting

Two fakes remain, and they can disagree. The risk is bounded and one-directional:
the preview's mocks are not evidence for anything (`docs/integration-testing.md`
says so), and the harness is now the tier that gates CI. The concrete cost is
that a behavior change made in the harness does not reach the preview, so the
preview drifts further from the app over time — which is an argument for doing
this sooner rather than a reason to reopen the decision.

## Outcome (landed)

**Decision: unify.** The preview runs on `DesktopHarnessState`. Everything the
analysis above predicted held, including that the branches needed deleting
rather than replacing.

### What changed

- `src/preview/` is new: `demoScenario.ts` (the demo project, declared through
  `ScenarioBuilder`), `liveJobs.ts` (timer-driven progress for the scriptable
  jobs), and `index.ts`, which builds the harness and installs it.
- `main.tsx` loads that module through a dynamic `import()` behind the
  `!inTauri()` check and installs the backend before it mounts `App`. It lands
  in its own chunk: the desktop bundle went from 339 kB to 326 kB, and the
  preview backend is a separate 59 kB file the desktop never fetches.
- `DesktopHarness` no longer imports `act`. It takes an `ActWrapper` defaulting
  to a passthrough, and `renderApp` calls `useActWrapper` with the
  `@testing-library/react` version before mounting.
- Every `!inTauri()` branch in `api.ts` and `chronologyApi.ts` is deleted, along
  with the demo data behind them and the three now-unreachable
  `!api.isDesktop()` branches in `App.tsx` (two "available in the desktop build"
  messages and one "browser preview only" message).

### Tests that now cover what the preview branches used to provide

- `shell.test.tsx`, `focus.test.tsx`, and `judgmentRail.test.tsx` boot against a
  declared scenario through `renderApp` instead of against `api.ts` demo data.
- `src/__independent__/preview-parity.independent.test.ts` is the parity charter:
  no environment branch survives in the adapters, the preview serves exactly the
  harness's command table and still throws on an unknown command, a proxy batch
  run through the preview's own door changes the assets it reports, the
  chronology it shows is declared rather than solved, and nothing a CUJ loads
  reaches preview code, a real timer, or ambient identity.
- `mediaUrl.test.ts` lost its "outside Tauri" case — there is no such case any
  more. The state it protected (no origin, so no URL) is still covered.

### Verified visually

`pnpm --dir apps/desktop build` succeeds, and the preview was driven at port
1420 in both `vite dev` and `vite preview` (the built output), through headless
Chromium with screenshots read back:

- boot: the demo project opens with its queue, notes, labels, AI chips,
  transcript tab, story ribbon lanes, and the card-detected banner;
- progress: "Make 4 proxies" animates 1/4 → 4/4 with an ETA and worker count,
  and the button then reads "Make 2 proxies" — the run's writes are observable,
  which is exactly what the old mock could not do;
- cancel: an analysis run stops mid-transcription and reports "Analysis stopped
  during transcribing; 0 clips analyzed", rather than claiming work it did not do;
- import: the smart-ingest dialog states 6 scanned / 3 new / 4 known, copies with
  a progress bar and a stop control, and the queue grows from 10 to 13;
- chronology and edit plan: both surfaces have content in the preview for the
  first time — sequences, phases, a diagnostic, a preview-then-apply clock
  correction with a declared post-edit ordering, and an edit plan with a
  validation finding.

### Reported, not fixed

With no media origin the stage's slate reads "No review proxy yet" even for a
clip that has a proxy. The slate infers the reason from the clip rather than
from why the URL is missing, so the desktop shows the same wrong sentence when
the loopback origin fails to bind. That is a production message gap this task
found and did not touch; fixing it is separate work.

## Handoff

Report the decision and its reasons, the branches removed or retained, and the
tests that now cover behavior the preview used to provide alone.
