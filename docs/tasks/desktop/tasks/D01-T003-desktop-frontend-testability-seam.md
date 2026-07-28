# D01-T003: Desktop frontend testability seam

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T003",
  "epic_id": "D01",
  "title": "Desktop frontend testability seam",
  "slug": "desktop-frontend-testability-seam",
  "status": "ready",
  "task_class": "foundation",
  "depends_on": ["D01-T002"],
  "principal_invariant": "Desktop review ordering and keyboard dispatch are exercisable by automated tests through modules the running application actually uses, with no change to observable application behavior.",
  "central_falsification": "Mutate one ordering rule and one key binding inside the extracted modules, run the app's own review flow, and confirm both the extracted-module tests fail and the running application changes behavior; a mutation that tests miss or that the app ignores proves the module is not the real seam.",
  "architecture_decisions_frozen": false,
  "load_bearing_seams": ["App.tsx render layer -> extracted pure review logic"],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/desktop/ARCHITECTURE.md", "AGENTS.md", "PRD.md"],
    "independent_tester": ["docs/tasks/desktop/ARCHITECTURE.md", "AGENTS.md"],
    "reviewer": ["docs/tasks/desktop/ARCHITECTURE.md", "AGENTS.md", "PRD.md"],
    "integrator": ["docs/tasks/desktop/ARCHITECTURE.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T003-desktop-frontend-testability-seam.md"],
    "implementer": [
      "apps/desktop/src/review/order.ts",
      "apps/desktop/src/review/keymap.ts",
      "apps/desktop/src/review/order.test.ts",
      "apps/desktop/src/review/keymap.test.ts",
      "apps/desktop/src/App.tsx",
      "apps/desktop/vitest.config.ts",
      "apps/desktop/src/test/setup.ts"
    ],
    "independent_tester": [
      "apps/desktop/src/__independent__/review-order.independent.test.ts",
      "apps/desktop/src/__independent__/review-keymap.independent.test.ts"
    ],
    "integrator": ["apps/desktop/package.json", "package.json", "docs/task-cycles/D01-T003/**"]
  },
  "forbidden_paths": ["core/**", "apps/web/**", "apps/server/**", "apps/cli/**", "packages/**", ".github/**", "apps/desktop/src-tauri/**", "apps/desktop/src/api.ts", "apps/desktop/src/types.ts"],
  "verification_commands": [
    "pnpm --dir apps/desktop check",
    "pnpm --dir apps/desktop test",
    "pnpm --dir apps/desktop build"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T003\\].*\\| complete \\|"
    },
    {
      "path": "apps/desktop/package.json",
      "regex": "\"test\"\\s*:\\s*\"vitest run\""
    },
    {
      "path": "package.json",
      "regex": "\"test:desktop\""
    }
  ]
}
-->

## Goal

Give `apps/desktop` a test runner and extract the review queue's pure logic out
of the render layer, so that the rest of this epic can add a calibration surface
and an edit-plan surface to a file that is currently 2,103 lines with zero tests.

This is the foundation task for every later desktop UI packet. It decides where
the testable boundary sits.

## Principal invariant

This task is correct when desktop review ordering and keyboard dispatch are
exercisable by automated tests through modules the running application actually
uses, with no change to observable application behavior.

Both halves matter. A module that is tested but not the one the app calls is
theater; a module the app calls but whose extraction changed behavior is a
regression.

## Decision authority

This packet owns the following decisions. No later packet may revisit them.

**What is extracted.** Exactly two modules, both pure — no React imports, no
`invoke`, no DOM access:

- `apps/desktop/src/review/order.ts` — the ordering and selection logic
  currently inline in `App.tsx` as `activeAssets` and `moveSelection`.
- `apps/desktop/src/review/keymap.ts` — the keyboard dispatch table currently
  inline in the `useEffect` keydown handler in `App.tsx`.

**What is not extracted.** Rendering, state hooks, effects, scroll management,
and everything touching `api.ts`. `App.tsx` keeps its structure and is edited
only to import from and delegate to the two new modules.

**Alternatives considered and rejected.**

| Alternative | Why rejected |
| --- | --- |
| Split `App.tsx` into a component tree first | Unbounded, unreviewable, and behavior-preserving component extraction is far riskier than pure-logic extraction. `AGENTS.md` §15 forbids reformatting large files. |
| Add only Playwright coverage of the desktop UI | Tauri is not a browser; Playwright against the Vite mock is explicitly not evidence per `AGENTS.md` §15, and slow end-to-end tests will not be run per-change. |
| Leave `App.tsx` alone and test through rendering only | Ordering and keyboard behavior would stay untestable without a DOM, which is where the actual chronology-critical logic lives. |
| Extract every pure helper in the file | Scope creep. Only the logic later packets depend on is in scope. |

**Criteria for accepting the seam:** a mutation to a rule inside an extracted
module must change both the module test result and the running application's
behavior. If either does not move, the seam is in the wrong place.

## Inputs and outputs

Inputs:

- `apps/desktop/src/App.tsx` as it stands, particularly `activeAssets`,
  `moveSelection`, `centerSelectedInChronology`, and the keydown `useEffect`.
- The `apps/web` test setup as the pattern to mirror: `vitest`, `jsdom`,
  `@testing-library/react`, `@testing-library/jest-dom`, `@testing-library/user-event`.

Outputs:

- The two extracted modules and their tests.
- A working `vitest` configuration for `apps/desktop` and a `test` script.
- A root `test:desktop` script so the gate and CI can invoke it.
- `App.tsx` delegating to the modules, behavior unchanged.

## Frozen decisions

- Mirror `apps/web`'s test stack exactly. Do not introduce a second test runner,
  a different assertion library, or a different jsdom configuration into this
  repository.
- The extraction is behavior-preserving. Bugs discovered in the extracted logic
  are recorded and flagged, not fixed here — fixing them silently would make the
  "no behavior change" claim untestable.
- `api.ts` remains the only module that calls `invoke`, per `AGENTS.md` §10. It is
  forbidden to this task.
- No new runtime dependency. The additions are dev dependencies only.
- Types come from the existing `apps/desktop/src/types.ts`, which is forbidden to
  edit; if the extracted modules need a type that does not exist there, that is a
  stop condition, not a licence to add one.

## Owned paths

The implementer owns the two modules, their co-located tests, the vitest config,
the test setup file, and the specific `App.tsx` call sites. The independent
tester owns a separate `__independent__` directory so their fixtures cannot be
confused with the implementer's. Both `package.json` manifests belong to the
integrator, because manifests are shared hot spots.

`apps/desktop/src-tauri/**`, `api.ts`, and `types.ts` are forbidden: this task is
about frontend testability, and reaching into the native layer or the DTO mirror
would make it cross a second seam.

## Required interface

Frozen for consumption by D01-T015, D01-T016, and D01-T023:

```ts
// apps/desktop/src/review/order.ts
export interface OrderOptions {
  removedVisible: boolean;
}

/** Returns the review queue in display order. Pure, total, and stable. */
export function orderAssets(assets: Asset[], options: OrderOptions): Asset[];

/** Index of assetId in ordered, or -1. */
export function indexOfAsset(ordered: Asset[], assetId: string | null): number;

/**
 * The selection after a move. Returns the current id when the move would leave
 * the queue, so selection is never lost at a boundary.
 */
export function moveSelection(
  ordered: Asset[],
  currentId: string | null,
  delta: number,
): string | null;

/**
 * The selection to restore after the queue is reordered. Prefers the same
 * asset; falls back to the nearest surviving neighbour by prior position.
 */
export function nextSelection(
  ordered: Asset[],
  previousId: string | null,
): string | null;
```

```ts
// apps/desktop/src/review/keymap.ts
export type ReviewAction =
  | { kind: "move"; delta: number }
  | { kind: "toggleRemoved" }
  | { kind: "zoom"; delta: number }
  | { kind: "centerSelection" };

export interface KeyEventLike {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  target: { tagName: string; isContentEditable: boolean } | null;
}

/** Null when the event is not a review shortcut, including while typing. */
export function resolveAction(event: KeyEventLike): ReviewAction | null;
```

`Asset` is imported from `apps/desktop/src/types.ts` unchanged. `KeyEventLike`
is structural on purpose so tests need no synthetic DOM events.

## Acceptance oracle

Behavior cases, supplied independently of the implementation:

1. `orderAssets` with `removedVisible: false` omits every asset whose `removed`
   is true, and preserves the relative order of the rest.
2. `orderAssets` is stable: called twice on the same input it returns the same
   order, and two assets never compare equal.
3. `moveSelection(ordered, firstId, -1)` returns `firstId`, not null and not the
   last item. The queue does not wrap.
4. `moveSelection(ordered, lastId, +1)` returns `lastId`.
5. `moveSelection(ordered, null, +1)` returns the first asset's id.
6. `nextSelection` returns the same id when the asset survives a reorder, and
   the nearest surviving neighbour by prior position when it does not.
7. `resolveAction` returns null when `target.tagName` is `INPUT` or `TEXTAREA`,
   or when `target.isContentEditable` is true — typing a note must never trigger
   a review shortcut.
8. `resolveAction` returns null when `metaKey` or `ctrlKey` is set, so system
   shortcuts are not shadowed.
9. Every key binding the current `App.tsx` handler implements resolves to the
   corresponding action, and no binding is silently dropped in the extraction.
10. `pnpm --dir apps/desktop build` succeeds and the app renders and navigates
    the review queue exactly as it did before the change.

## Independent test charter

Falsify the claim that the extracted modules are the real seam and that behavior
is unchanged.

- Write your own fixtures in `apps/desktop/src/__independent__/`. Do not import
  the implementer's fixtures or test helpers.
- Build an asset list with duplicate `best_time` values, a null `best_time`, a
  non-ASCII filename, and two assets whose filenames differ only by counter, and
  assert `orderAssets` is still total and stable across 32 shuffles of the input
  array.
- Assert `moveSelection` never returns an id absent from `ordered`, for every
  delta in −3..3 and every starting position including null.
- Assert `resolveAction` returns null for a keydown whose target is a
  contenteditable div, and for every single-letter key while `metaKey` is set.
- Prove the seam is load-bearing: temporarily invert the comparison in
  `order.ts` and the direction of one binding in `keymap.ts`, then confirm both
  the module tests fail and the built application visibly changes. Revert both
  mutations and record the result. A mutation the app does not notice means
  `App.tsx` still has its own copy of the logic.
- Diff `App.tsx` and confirm the extraction removed the inline logic rather than
  duplicating it. A surviving inline implementation is a finding.
- Confirm `apps/desktop/src/api.ts` and `types.ts` are untouched.

## Review charter

Risk domain: a testability seam that either changes behavior or does not
actually sit on the path the application executes.

Questions:

1. Does `App.tsx` still contain a second copy of any extracted rule?
2. Did any behavior change — key bindings, ordering, boundary handling, selection
   at the ends of the queue?
3. Are the extracted modules genuinely pure? Any React import, `invoke` call,
   `document` reference, `Date.now()`, or `Math.random()` in them is a defect.
4. Does the vitest configuration match `apps/web`'s, or has a second convention
   been introduced?
5. Was the extraction used as cover for unrelated refactoring or reformatting of
   `App.tsx`?
6. Is the exported interface exactly the one this packet froze, so later packets
   can rely on it?

Severity rules: behavior change, duplicated logic, impurity in an extracted
module, or an interface deviation is blocking. Naming and file-layout preferences
are non-blocking notes.

Smallest acceptable remedy: for a behavior change, restore the original behavior
and add the test that would have caught the deviation. For duplication, delete
the inline copy.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Splitting `App.tsx` into components.
- Fixing any bug found in the extracted logic.
- Adding Playwright or any end-to-end runner to `apps/desktop`.
- Testing anything that calls `invoke`.
- Touching the Tauri command layer, `api.ts`, or `types.ts`.
- Styling, accessibility, or layout changes.

## Stop conditions

Stop and escalate when:

- extracting the keyboard map cannot be done without changing a binding's
  behavior, which means the current handler has order-dependent behavior that
  needs a design decision;
- the extracted modules need a type that `types.ts` does not export;
- `vitest` in `apps/desktop` conflicts with the Tauri or Vite configuration in a
  way that requires changing build configuration beyond adding a test config;
- the extraction turns out to require touching `api.ts` or the native layer;
- `App.tsx`'s inline logic is so entangled with hook state that a pure
  extraction is impossible without restructuring the component.

## Commands

```sh
pnpm install
pnpm --dir apps/desktop check
pnpm --dir apps/desktop test
pnpm --dir apps/desktop build
pnpm test:desktop
```

Expected result: `check` reports no TypeScript errors, `test` runs the new suites
and passes, `build` succeeds, and `pnpm test:desktop` from the repository root
runs the same suite.

## Handoff

Report the two module interfaces as implemented, the `App.tsx` diff summary, the
dev dependencies added, the test counts, and the mutation-test result proving the
seam is load-bearing. State explicitly that no behavior changed, and list any bug
noticed in the extracted logic without fixing it. List any stop condition
encountered. The implementer does not approve this task.
