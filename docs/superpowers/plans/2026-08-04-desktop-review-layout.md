# Desktop Review Layout Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the desktop main view to fit a 13" MacBook Air, per
`docs/superpowers/specs/2026-08-04-desktop-review-layout-design.md`.

**Architecture:** `App.tsx` (2685 lines) keeps state, effects and handlers but
sheds its render tree into focused components under `src/components/`. Every
piece of behaviour that can be expressed as a pure function — ribbon geometry,
presentation state, collapsed-section summaries, keyboard resolution — moves
into `src/review/` modules unit-tested in isolation, following the established
`review/order.ts` + `review/order.test.ts` pattern. Structural guarantees the
spec demands (notes always visible, no machine content in the human rail) are
enforced by component tests rather than by review discipline.

**Tech Stack:** React 19, TypeScript 5.9, Vite 7, Vitest 3 + jsdom +
@testing-library/react (installed and configured, currently unused), Tauri 2,
plain CSS in `src/styles.css`.

## Global Constraints

- Target viewport is 1440 x 900. `html, body, #root { min-width: 980px }` stands.
- Spec §2 rules 1–6 are invariants. Rule 1 (right rail holds human-authored
  content only, exceptions: AI label strip and the `⚠` chip) and rule 3 (notes
  fixed geometry within a presentation) are enforced by tests, not review.
- Type scale is exactly: 10/12, 11/14, 12/16, 13/18, 15/22, 17/22. No other
  size. 15/22 is used by the notes textarea and nothing else.
- Spacing scale is exactly: 2, 4, 6, 8, 12, 16, 24. Nothing above 24.
- No element carries both a border and a background fill. Max nesting depth 2:
  a bordered element may not contain another bordered element.
- Radii: 3px for chips/inputs/buttons, 6px for dialogs. Nothing else rounds.
- Palette stays graphite/amber as defined in `:root`. No new colour systems.
- Every `var(--name)` reference must resolve to a property defined in `:root`
  or carry an inline fallback.
- Existing behaviour outside the spec's scope must not change: ingest, proxy
  and thumbnail generation, analysis chain, sync, pairing, settings, and all
  modals keep their current behaviour.
- Verify with `pnpm --dir apps/desktop test`, `pnpm --dir apps/desktop check`,
  and `pnpm --dir apps/desktop build`. Baseline before this plan: 122 tests
  passing across 4 files.

## Blocking decision resolved: the `T` key collision

The spec assigns `T` to "collapse ribbon". `T` is currently bound to
`makeMissingThumbnails` (`App.tsx:758`), advertised as `<kbd>T</kbd>` in the
media menu (`App.tsx:1370`) and in the shortcuts modal.

**Resolution: thumbnails moves to `⇧G`, and `T` becomes the ribbon toggle.**
Rationale from spec rule 4 — thumbnail generation is a once-per-project setup
action and does not earn a bare key, while the ribbon toggle is pressed many
times per session. Proxies keep `⇧P`, analysis keeps `A`, import keeps `M`.
Task 4 makes this change; Task 11 updates the shortcuts modal and the media
menu hint.

No other new key collides. `F`, `H`, `D`, `B`, `I` are unbound today.

## File Structure

**Created:**

| Path | Responsibility |
| --- | --- |
| `src/review/layout.ts` | Pure geometry: ribbon height from lane count, grid templates |
| `src/review/layout.test.ts` | Unit tests for the above |
| `src/review/presentation.ts` | Pure state machine: Review/Focus, overlay flags, transitions |
| `src/review/presentation.test.ts` | Unit tests for the above |
| `src/review/summaries.ts` | Pure one-line summaries for collapsed sections and shelf tab badges |
| `src/review/summaries.test.ts` | Unit tests for the above |
| `src/review/tokens.test.ts` | Asserts every `var()` in styles.css resolves |
| `src/components/TopBar.tsx` | Topbar: project menu, mode switcher, operation slot, actions |
| `src/components/QueuePanel.tsx` | Search, filters, clip list |
| `src/components/VideoStage.tsx` | The frame, buffering hairline, placeholder states |
| `src/components/TransportBar.tsx` | Permanent transport, scrubber with markers |
| `src/components/EvidenceShelf.tsx` | Tab strip + Transcript/Description/Analysis/File panes |
| `src/components/JudgmentPanel.tsx` | Clip header, notes, labels, AI suggestion strip |
| `src/components/StoryRibbon.tsx` | Full-width ribbon, adaptive lanes, merged-lane variant |
| `src/components/FocusView.tsx` | The Focus presentation (spec §3.9) |
| `src/components/__tests__/*.test.tsx` | Component tests, one file per component under test |

**Modified:**

| Path | Change |
| --- | --- |
| `src/App.tsx` | Sheds render tree; keeps state, effects, handlers; adds mode + presentation routing |
| `src/styles.css` | Layout rewrite, token definitions, defect fixes, density pass |
| `src/review/keymap.ts` | New actions; `T` reassignment |
| `src/review/keymap.test.ts` | Tests for new actions |
| `src/__independent__/review-keymap.independent.test.ts` | Update where behaviour changed |

**Deleted:** no files. `CalibrationPanel.tsx` and `EditPlanPanel.tsx` are kept
and rehosted into modes (Task 9).

---

### Task 1: CSS token audit and defect fixes

Spec §1.3, §7. No layout change — this task fixes what is broken today and
establishes the tokens later tasks consume.

**Files:**
- Create: `apps/desktop/src/review/tokens.ts`
- Create: `apps/desktop/src/review/tokens.test.ts`
- Modify: `apps/desktop/src/styles.css` (`:root` block at lines 1-22;
  `.sync-indicator` 430-447; `.button:disabled` 79; `.project-switcher select`
  56-63; `.chronology-drawer` 582-587; `.sync-diag-*` 468-578;
  `.calibration-*` / `.edit-plan-*` 600-661)

**Interfaces:**
- Consumes: nothing.
- Produces: `collectCustomProperties(css: string): { defined: Set<string>;
  referenced: Array<{ name: string; hasFallback: boolean }> }` from
  `src/review/tokens.ts`, used only by its own test.

- [ ] **Step 1: Write the failing test**

Create `apps/desktop/src/review/tokens.test.ts`:

```ts
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { collectCustomProperties } from "./tokens";

const css = readFileSync(
  fileURLToPath(new URL("../styles.css", import.meta.url)),
  "utf8",
);

describe("collectCustomProperties", () => {
  it("reports properties defined in a rule block", () => {
    const result = collectCustomProperties(":root { --ink: #fff; --line: #222; }");
    expect(result.defined.has("--ink")).toBe(true);
    expect(result.defined.has("--line")).toBe(true);
  });

  it("reports references and whether each carries a fallback", () => {
    const result = collectCustomProperties("a { color: var(--ink); border-color: var(--line, #222); }");
    expect(result.referenced).toEqual([
      { name: "--ink", hasFallback: false },
      { name: "--line", hasFallback: true },
    ]);
  });

  it("does not treat a definition as a reference", () => {
    const result = collectCustomProperties(":root { --ink: #fff; }");
    expect(result.referenced).toEqual([]);
  });
});

describe("styles.css", () => {
  it("references no custom property that is neither defined nor given a fallback", () => {
    const { defined, referenced } = collectCustomProperties(css);
    const unresolved = referenced
      .filter((ref) => !ref.hasFallback && !defined.has(ref.name))
      .map((ref) => ref.name);
    expect([...new Set(unresolved)]).toEqual([]);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test tokens`
Expected: FAIL — `Cannot find module './tokens'`.

- [ ] **Step 3: Write the parser**

Create `apps/desktop/src/review/tokens.ts`:

```ts
export interface CustomPropertyUsage {
  defined: Set<string>;
  referenced: Array<{ name: string; hasFallback: boolean }>;
}

/**
 * Scans CSS text for custom-property definitions and references.
 *
 * A reference without a definition and without a fallback is invalid at
 * computed-value time: the declaration falls back to the property's initial
 * value, which silently removes borders and resets colours rather than
 * failing loudly. This exists so a test can forbid that.
 */
export function collectCustomProperties(css: string): CustomPropertyUsage {
  const defined = new Set<string>();
  for (const match of css.matchAll(/(--[\w-]+)\s*:/g)) {
    defined.add(match[1]);
  }

  const referenced: Array<{ name: string; hasFallback: boolean }> = [];
  for (const match of css.matchAll(/var\(\s*(--[\w-]+)\s*(,)?/g)) {
    referenced.push({ name: match[1], hasFallback: Boolean(match[2]) });
  }

  return { defined, referenced };
}
```

- [ ] **Step 4: Run the test to see the real failure**

Run: `pnpm --dir apps/desktop test tokens`
Expected: the three parser tests PASS; the `styles.css` test FAILS listing
`--border-default`, `--color-text-muted`, `--color-accent`,
`--color-surface-raised`.

- [ ] **Step 5: Fix the stylesheet**

In `:root` (`styles.css:7-21`), append the density scale and the alias tokens
that `.sync-indicator` needs:

```css
  --line-soft: #22292e;
  --focus: var(--amber);
  /* Sizes below are the whole type scale. Nothing may introduce another. */
  --t-micro: 10px;   --lh-micro: 12px;
  --t-meta: 11px;    --lh-meta: 14px;
  --t-chip: 12px;    --lh-chip: 16px;
  --t-body: 13px;    --lh-body: 18px;
  --t-notes: 15px;   --lh-notes: 22px;
  --t-title: 17px;   --lh-title: 22px;
```

Rewrite `.sync-indicator` (`styles.css:430-447`) onto the real palette:

```css
.sync-indicator {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 4px 10px;
  border-radius: 3px;
  background: transparent;
  border: 1px solid var(--line-strong);
  cursor: pointer;
  font-size: var(--t-chip);
  color: var(--muted);
  transition: border-color 120ms ease, color 120ms ease;
}

.sync-indicator:hover { border-color: var(--quiet); color: var(--ink); }
```

Replace `.button:disabled` (`styles.css:79`) so the border stops dimming with
the label:

```css
.button:disabled { color: var(--quiet); border-color: var(--line); background: var(--panel); cursor: not-allowed; }
.button.primary:disabled { color: #6b5a41; background: #4a3a22; border-color: #4a3a22; }
```

Change `.project-switcher select` (`styles.css:60`) from `var(--line)` to
`var(--line-strong)` so it matches adjacent buttons.

In `.chronology-drawer` (`styles.css:583`) replace `var(--border, #2a2a33)`
with `var(--line)`. In the `.sync-diag-*` block (`styles.css:468-578`) replace
every `var(--border-default)` with `var(--line)`, every
`var(--color-text-muted, …)` with `var(--muted)`, every
`var(--color-surface-2, #1e1e2e)` with `var(--panel-raised)`, every
`var(--color-border, #313244)` with `var(--line)`, and every
`var(--color-text, #cdd6f4)` with `var(--ink)`. In the `.calibration-*` /
`.edit-plan-*` block (`styles.css:600-661`) convert every `rem` value to its px
equivalent at 16px base (`0.75rem` → 12px, `1rem` → 16px, `0.85rem` → 13px,
`0.4rem` → 4px, `0.5rem` → 8px, `0.35rem` → 4px, `0.15rem` → 2px,
`0.78rem` → 12px, `0.75rem` → 12px) and replace `var(--border, #2a2a33)` with
`var(--line)`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `pnpm --dir apps/desktop test tokens`
Expected: PASS, 4 tests.

- [ ] **Step 7: Verify nothing else broke**

Run: `pnpm --dir apps/desktop test && pnpm --dir apps/desktop check`
Expected: 126 tests pass, no TypeScript errors.

- [ ] **Step 8: Commit**

```bash
git add apps/desktop/src/review/tokens.ts apps/desktop/src/review/tokens.test.ts apps/desktop/src/styles.css
git commit -m "fix(desktop): resolve undefined CSS custom properties

.sync-indicator referenced four properties never defined in :root, so its
border collapsed to the initial value and its colour inherited at full
brightness -- it rendered unlike every other topbar control. A test now
forbids an unresolved var() reference.

Disabled buttons dimmed their border along with their label via opacity,
so a disabled Proxies ready button and the enabled toggle beside it showed
different outlines despite sharing a class."
```

---

### Task 2: Ribbon geometry as a pure module

Spec §3.1, §3.8. The ribbon's height depends on lane count; today
`repeat(5, 22px)` is hardcoded in two places, leaving dead rows below five
cameras and clipping the sixth.

**Files:**
- Create: `apps/desktop/src/review/layout.ts`
- Create: `apps/desktop/src/review/layout.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces, all from `src/review/layout.ts`:
  - `RIBBON_MAX_LANES = 5`
  - `RIBBON_LANE_HEIGHT = 20`
  - `RIBBON_CHROME_HEIGHT = 56`
  - `RIBBON_COLLAPSED_HEIGHT = 28`
  - `ribbonHeight(laneCount: number, collapsed: boolean): number`
  - `ribbonLaneRows(laneCount: number): string`
  - `visibleLaneCount(laneCount: number): number`

- [ ] **Step 1: Write the failing test**

Create `apps/desktop/src/review/layout.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import {
  RIBBON_COLLAPSED_HEIGHT,
  ribbonHeight,
  ribbonLaneRows,
  visibleLaneCount,
} from "./layout";

describe("ribbonHeight", () => {
  it("is chrome plus one row per lane at or below the cap", () => {
    expect(ribbonHeight(1, false)).toBe(76);
    expect(ribbonHeight(2, false)).toBe(96);
    expect(ribbonHeight(5, false)).toBe(156);
  });

  it("caps at five lanes so a six-camera project scrolls instead of growing", () => {
    expect(ribbonHeight(6, false)).toBe(156);
    expect(ribbonHeight(12, false)).toBe(156);
  });

  it("is the collapsed height regardless of lane count when collapsed", () => {
    expect(ribbonHeight(2, true)).toBe(RIBBON_COLLAPSED_HEIGHT);
    expect(ribbonHeight(9, true)).toBe(RIBBON_COLLAPSED_HEIGHT);
  });

  it("renders chrome only when there are no lanes at all", () => {
    expect(ribbonHeight(0, false)).toBe(56);
  });

  it("treats a negative or fractional lane count as zero lanes", () => {
    expect(ribbonHeight(-3, false)).toBe(56);
    expect(ribbonHeight(2.7, false)).toBe(96);
  });
});

describe("visibleLaneCount", () => {
  it("never exceeds the cap and never goes below zero", () => {
    expect(visibleLaneCount(0)).toBe(0);
    expect(visibleLaneCount(3)).toBe(3);
    expect(visibleLaneCount(5)).toBe(5);
    expect(visibleLaneCount(7)).toBe(5);
    expect(visibleLaneCount(-1)).toBe(0);
  });
});

describe("ribbonLaneRows", () => {
  it("emits one track per actual lane, not a hardcoded five", () => {
    expect(ribbonLaneRows(2)).toBe("repeat(2, 20px)");
    expect(ribbonLaneRows(7)).toBe("repeat(7, 20px)");
  });

  it("emits nothing when there are no lanes", () => {
    expect(ribbonLaneRows(0)).toBe("");
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test layout`
Expected: FAIL — `Cannot find module './layout'`.

- [ ] **Step 3: Write the implementation**

Create `apps/desktop/src/review/layout.ts`:

```ts
/** Story-ribbon geometry (spec §3.8). */

export const RIBBON_MAX_LANES = 5;
export const RIBBON_LANE_HEIGHT = 20;
/** Control row (22) + trip-context band (20) + position numerals (14). */
export const RIBBON_CHROME_HEIGHT = 56;
export const RIBBON_COLLAPSED_HEIGHT = 28;

function laneFloor(laneCount: number) {
  return Math.max(0, Math.floor(Number.isFinite(laneCount) ? laneCount : 0));
}

/**
 * Lanes actually given a track. Past the cap the lane stack scrolls
 * internally rather than growing the ribbon, so the main row's height stays
 * predictable no matter how many cameras a project holds.
 */
export function visibleLaneCount(laneCount: number) {
  return Math.min(RIBBON_MAX_LANES, laneFloor(laneCount));
}

export function ribbonHeight(laneCount: number, collapsed: boolean) {
  if (collapsed) return RIBBON_COLLAPSED_HEIGHT;
  return RIBBON_CHROME_HEIGHT + visibleLaneCount(laneCount) * RIBBON_LANE_HEIGHT;
}

/** The `grid-template-rows` fragment for the lane tracks. */
export function ribbonLaneRows(laneCount: number) {
  const lanes = laneFloor(laneCount);
  return lanes === 0 ? "" : `repeat(${lanes}, ${RIBBON_LANE_HEIGHT}px)`;
}
```

Note `ribbonLaneRows` uses the true lane count (the canvas renders every lane
and scrolls) while `ribbonHeight` uses the capped count (the viewport shows at
most five).

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm --dir apps/desktop test layout`
Expected: PASS, 9 tests.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/review/layout.ts apps/desktop/src/review/layout.test.ts
git commit -m "feat(desktop): ribbon geometry derived from lane count

repeat(5, 22px) was hardcoded in .story-labels and .story-canvas, so a
two-camera project rendered three dead rows and a sixth camera was clipped
by overflow: hidden."
```

---

### Task 3: Presentation state as a pure module

Spec §3.9, §6. Review and Focus are presentations of the same working set;
Chronology and Edit plan are modes. Overlay visibility is keyboard-driven and
never derived from hover or playback.

**Files:**
- Create: `apps/desktop/src/review/presentation.ts`
- Create: `apps/desktop/src/review/presentation.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces, all from `src/review/presentation.ts`:
  - `type Mode = "review" | "chronology" | "editPlan"`
  - `type Presentation = "review" | "focus"`
  - `interface ViewState { mode: Mode; presentation: Presentation; overlays: boolean; ribbon: boolean; rightStack: boolean; shelf: boolean }`
  - `INITIAL_VIEW: ViewState`
  - `type ViewAction = { kind: "enterFocus"; overlays: boolean } | { kind: "exitFocus" } | { kind: "setMode"; mode: Mode } | { kind: "toggleOverlays" } | { kind: "toggleRibbon" } | { kind: "toggleRightStack" } | { kind: "toggleShelf" }`
  - `reduceView(state: ViewState, action: ViewAction): ViewState`
  - `focusesNotesOnEntry(action: ViewAction): boolean`

- [ ] **Step 1: Write the failing test**

Create `apps/desktop/src/review/presentation.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { INITIAL_VIEW, focusesNotesOnEntry, reduceView, type ViewState } from "./presentation";

const focused: ViewState = reduceView(INITIAL_VIEW, { kind: "enterFocus", overlays: true });

describe("initial state", () => {
  it("opens in Review with the ribbon shown and the shelf collapsed", () => {
    expect(INITIAL_VIEW).toEqual({
      mode: "review",
      presentation: "review",
      overlays: true,
      ribbon: true,
      rightStack: false,
      shelf: false,
    });
  });
});

describe("entering Focus", () => {
  it("switches presentation and keeps the mode as review", () => {
    expect(focused.presentation).toBe("focus");
    expect(focused.mode).toBe("review");
  });

  it("can enter with overlays hidden, which is what the expand control does", () => {
    const bare = reduceView(INITIAL_VIEW, { kind: "enterFocus", overlays: false });
    expect(bare.presentation).toBe("focus");
    expect(bare.overlays).toBe(false);
  });

  it("focuses the notes field only when entering with overlays shown", () => {
    expect(focusesNotesOnEntry({ kind: "enterFocus", overlays: true })).toBe(true);
    expect(focusesNotesOnEntry({ kind: "enterFocus", overlays: false })).toBe(false);
    expect(focusesNotesOnEntry({ kind: "exitFocus" })).toBe(false);
  });
});

describe("leaving Focus", () => {
  it("returns to the review presentation and restores overlay visibility", () => {
    const bare = reduceView(INITIAL_VIEW, { kind: "enterFocus", overlays: false });
    const back = reduceView(bare, { kind: "exitFocus" });
    expect(back.presentation).toBe("review");
    expect(back.overlays).toBe(true);
  });

  it("preserves the ribbon and shelf toggles across the round trip", () => {
    const tweaked = reduceView(reduceView(INITIAL_VIEW, { kind: "toggleRibbon" }), { kind: "toggleShelf" });
    const roundTrip = reduceView(reduceView(tweaked, { kind: "enterFocus", overlays: true }), { kind: "exitFocus" });
    expect(roundTrip.ribbon).toBe(tweaked.ribbon);
    expect(roundTrip.shelf).toBe(tweaked.shelf);
  });
});

describe("modes", () => {
  it("leaves Focus when switching to another mode", () => {
    const chronology = reduceView(focused, { kind: "setMode", mode: "chronology" });
    expect(chronology.mode).toBe("chronology");
    expect(chronology.presentation).toBe("review");
  });

  it("returns to Review rather than to Focus", () => {
    const chronology = reduceView(focused, { kind: "setMode", mode: "chronology" });
    const back = reduceView(chronology, { kind: "setMode", mode: "review" });
    expect(back.presentation).toBe("review");
  });
});

describe("toggles", () => {
  it("toggleRibbon and toggleShelf flip exactly one flag each", () => {
    const ribbon = reduceView(INITIAL_VIEW, { kind: "toggleRibbon" });
    expect(ribbon.ribbon).toBe(false);
    expect({ ...ribbon, ribbon: true }).toEqual(INITIAL_VIEW);

    const shelf = reduceView(INITIAL_VIEW, { kind: "toggleShelf" });
    expect(shelf.shelf).toBe(true);
    expect({ ...shelf, shelf: false }).toEqual(INITIAL_VIEW);
  });

  it("toggleRightStack and toggleOverlays flip exactly one flag each", () => {
    const stack = reduceView(focused, { kind: "toggleRightStack" });
    expect(stack.rightStack).toBe(true);
    expect({ ...stack, rightStack: false }).toEqual(focused);

    const hidden = reduceView(focused, { kind: "toggleOverlays" });
    expect(hidden.overlays).toBe(false);
    expect({ ...hidden, overlays: true }).toEqual(focused);
  });

  it("ignores toggleOverlays and toggleRightStack outside Focus", () => {
    expect(reduceView(INITIAL_VIEW, { kind: "toggleOverlays" })).toEqual(INITIAL_VIEW);
    expect(reduceView(INITIAL_VIEW, { kind: "toggleRightStack" })).toEqual(INITIAL_VIEW);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test presentation`
Expected: FAIL — `Cannot find module './presentation'`.

- [ ] **Step 3: Write the implementation**

Create `apps/desktop/src/review/presentation.ts`:

```ts
/**
 * View state for the desktop main screen (spec §3.9, §6).
 *
 * Review and Focus are two presentations of the same working set — same clip,
 * filter, search and queue order. Chronology and Edit plan are modes with
 * their own surfaces and no Focus presentation.
 *
 * Overlay visibility lives here, and only explicit actions change it: nothing
 * in this module reacts to hover or to playback state, because a control that
 * appears on mouse-entry is one you cannot aim at.
 */

export type Mode = "review" | "chronology" | "editPlan";
export type Presentation = "review" | "focus";

export interface ViewState {
  mode: Mode;
  presentation: Presentation;
  /** Focus only: whether any overlay is drawn over the frame. */
  overlays: boolean;
  ribbon: boolean;
  /** Focus only: the optional labels + transcript column. Off by default. */
  rightStack: boolean;
  /** Review only: the evidence shelf. Collapsed by default. */
  shelf: boolean;
}

export const INITIAL_VIEW: ViewState = {
  mode: "review",
  presentation: "review",
  overlays: true,
  ribbon: true,
  rightStack: false,
  shelf: false,
};

export type ViewAction =
  | { kind: "enterFocus"; overlays: boolean }
  | { kind: "exitFocus" }
  | { kind: "setMode"; mode: Mode }
  | { kind: "toggleOverlays" }
  | { kind: "toggleRibbon" }
  | { kind: "toggleRightStack" }
  | { kind: "toggleShelf" };

export function reduceView(state: ViewState, action: ViewAction): ViewState {
  switch (action.kind) {
    case "enterFocus":
      return { ...state, presentation: "focus", overlays: action.overlays };
    case "exitFocus":
      // Overlay visibility is a Focus-local concern; leaving resets it so the
      // next entry is never mysteriously bare.
      return { ...state, presentation: "review", overlays: true };
    case "setMode":
      return { ...state, mode: action.mode, presentation: "review", overlays: true };
    case "toggleOverlays":
      if (state.presentation !== "focus") return state;
      return { ...state, overlays: !state.overlays };
    case "toggleRightStack":
      if (state.presentation !== "focus") return state;
      return { ...state, rightStack: !state.rightStack };
    case "toggleRibbon":
      return { ...state, ribbon: !state.ribbon };
    case "toggleShelf":
      return { ...state, shelf: !state.shelf };
  }
}

/**
 * Entering with `F` puts the caret in the notes field, since writing is what
 * you came for. Entering with the expand control starts bare, so focusing an
 * invisible field would strand the caret.
 */
export function focusesNotesOnEntry(action: ViewAction) {
  return action.kind === "enterFocus" && action.overlays;
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm --dir apps/desktop test presentation`
Expected: PASS, 11 tests.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/review/presentation.ts apps/desktop/src/review/presentation.test.ts
git commit -m "feat(desktop): pure view state for modes and the Focus presentation"
```

---

### Task 4: Keyboard map for the new actions

Spec §8. Extends `resolveAction` with the spec's new keys and reassigns `T`.

**Files:**
- Modify: `apps/desktop/src/review/keymap.ts`
- Modify: `apps/desktop/src/review/keymap.test.ts`
- Modify: `apps/desktop/src/__independent__/review-keymap.independent.test.ts`

**Interfaces:**
- Consumes: nothing.
- Produces: `ReviewAction` gains these members —
  `{ kind: "toggleFocus" }`, `{ kind: "toggleOverlays" }`,
  `{ kind: "toggleShelf" }`, `{ kind: "toggleRibbon" }`,
  `{ kind: "ribbonOverlay" }`, `{ kind: "jumpBestMoment" }`,
  `{ kind: "editTitle" }`, `{ kind: "setMode"; mode: "review" | "chronology" | "editPlan" }`.

- [ ] **Step 1: Write the failing test**

Append to `apps/desktop/src/review/keymap.test.ts`:

```ts
describe("presentation shortcuts", () => {
  function key(k: string, mods: Partial<Pick<KeyEventLike, "metaKey" | "shiftKey">> = {}): KeyEventLike {
    return {
      key: k,
      metaKey: mods.metaKey ?? false,
      ctrlKey: false,
      altKey: false,
      shiftKey: mods.shiftKey ?? false,
      target: null,
    };
  }

  it("maps F to toggleFocus and H to toggleOverlays", () => {
    expect(resolveAction(key("f"))).toEqual({ kind: "toggleFocus" });
    expect(resolveAction(key("h"))).toEqual({ kind: "toggleOverlays" });
  });

  it("maps D to the shelf and bare T to the ribbon", () => {
    expect(resolveAction(key("d"))).toEqual({ kind: "toggleShelf" });
    expect(resolveAction(key("t"))).toEqual({ kind: "toggleRibbon" });
  });

  it("maps Shift+T to the expanded ribbon overlay, distinct from bare T", () => {
    expect(resolveAction(key("T", { shiftKey: true }))).toEqual({ kind: "ribbonOverlay" });
  });

  it("maps B to the best moment and I to editing the title", () => {
    expect(resolveAction(key("b"))).toEqual({ kind: "jumpBestMoment" });
    expect(resolveAction(key("i"))).toEqual({ kind: "editTitle" });
  });

  it("maps Cmd+1/2/3 to modes, which the plain digits do not", () => {
    expect(resolveAction(key("1", { metaKey: true }))).toEqual({ kind: "setMode", mode: "review" });
    expect(resolveAction(key("2", { metaKey: true }))).toEqual({ kind: "setMode", mode: "chronology" });
    expect(resolveAction(key("3", { metaKey: true }))).toEqual({ kind: "setMode", mode: "editPlan" });
    expect(resolveAction(key("1"))).toBeNull();
  });

  it("still returns null for every new key while a text field has focus", () => {
    for (const k of ["f", "h", "d", "t", "b", "i"]) {
      expect(
        resolveAction({
          key: k,
          metaKey: false,
          ctrlKey: false,
          altKey: false,
          shiftKey: false,
          target: { tagName: "TEXTAREA", isContentEditable: false },
        }),
      ).toBeNull();
    }
  });

  it("leaves Cmd+1 unhandled while a text field has focus", () => {
    expect(
      resolveAction({
        key: "1",
        metaKey: true,
        ctrlKey: false,
        altKey: false,
        shiftKey: false,
        target: { tagName: "INPUT", isContentEditable: false },
      }),
    ).toBeNull();
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test keymap`
Expected: FAIL — every new assertion returns `null`.

- [ ] **Step 3: Extend the keymap**

In `apps/desktop/src/review/keymap.ts`, widen the union:

```ts
export type ReviewAction =
  | { kind: "move"; delta: number }
  | { kind: "toggleRemoved" }
  | { kind: "zoom"; delta: number }
  | { kind: "centerSelection" }
  | { kind: "toggleFocus" }
  | { kind: "toggleOverlays" }
  | { kind: "toggleShelf" }
  | { kind: "toggleRibbon" }
  | { kind: "ribbonOverlay" }
  | { kind: "jumpBestMoment" }
  | { kind: "editTitle" }
  | { kind: "setMode"; mode: "review" | "chronology" | "editPlan" };
```

Update the doc comment's scope paragraph to say the module now also owns the
presentation and mode shortcuts, and that media generation, search focus,
label/notes focus, help and project actions remain in `App.tsx`.

Replace the body after the editable-target guard. The Meta guard must now
admit `⌘1/2/3` before rejecting the rest:

```ts
  const key = event.key.toLocaleLowerCase();

  // Modes are the only Meta combinations this module claims; everything else
  // Meta-held belongs to the OS or to App.tsx's explicit handlers.
  if (event.metaKey || event.ctrlKey) {
    if (event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey) {
      if (key === "1") return { kind: "setMode", mode: "review" };
      if (key === "2") return { kind: "setMode", mode: "chronology" };
      if (key === "3") return { kind: "setMode", mode: "editPlan" };
    }
    return null;
  }

  if (event.key === "ArrowDown" || key === "j") return { kind: "move", delta: 1 };
  if (event.key === "ArrowUp" || key === "k") return { kind: "move", delta: -1 };
  if (key === "r") return { kind: "toggleRemoved" };
  if (key === "[") return { kind: "zoom", delta: -1 };
  if (key === "]") return { kind: "zoom", delta: 1 };
  if (key === "c") return { kind: "centerSelection" };
  if (key === "f") return { kind: "toggleFocus" };
  if (key === "h") return { kind: "toggleOverlays" };
  if (key === "d") return { kind: "toggleShelf" };
  // Bare T collapses the ribbon; Shift+T expands it as an overlay. Thumbnail
  // generation moved to Shift+G: it is a once-per-project setup action and
  // does not earn a bare key (spec rule 4).
  if (key === "t") return event.shiftKey ? { kind: "ribbonOverlay" } : { kind: "toggleRibbon" };
  if (key === "b") return { kind: "jumpBestMoment" };
  if (key === "i") return { kind: "editTitle" };

  return null;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm --dir apps/desktop test keymap`
Expected: PASS. The independent suite may now fail — proceed to step 5.

- [ ] **Step 5: Update the independent suite for the changed contract**

`src/__independent__/review-keymap.independent.test.ts` asserts that every
non-listed key returns `null` and that all Meta combinations return `null`.
Both contracts changed. Update only the assertions that the spec changed —
add `f`, `h`, `d`, `t`, `b`, `i` to the handled set, and narrow the Meta
assertion to exclude `⌘1/2/3`. Leave the file's independent character intact:
do not copy fixtures from `keymap.test.ts`.

- [ ] **Step 6: Rewire App.tsx's handler**

In `App.tsx:758`, delete the `else if (key === "t")` thumbnails branch and
re-add it as `⇧G`:

```ts
      } else if (key === "g" && event.shiftKey) {
        void makeMissingThumbnails();
      }
```

In the `reviewAction` switch (`App.tsx:715-735`), add cases for the new kinds.
For this task, wire only the ones with existing behaviour — `jumpBestMoment`
seeks to `observation?.analysis.bestMomentStartMs`, and `editTitle` focuses
`.clip-title-field`. The presentation actions get `break` with a
`// wired in Task 9/10` comment; Tasks 9 and 10 replace them.

- [ ] **Step 7: Run the full suite**

Run: `pnpm --dir apps/desktop test && pnpm --dir apps/desktop check`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add apps/desktop/src/review/keymap.ts apps/desktop/src/review/keymap.test.ts apps/desktop/src/__independent__/review-keymap.independent.test.ts apps/desktop/src/App.tsx
git commit -m "feat(desktop): keyboard map for presentations and modes

T moves from thumbnail generation to the ribbon toggle. Thumbnails is a
once-per-project setup action and does not earn a bare key; it becomes
Shift+G. The ribbon toggle is pressed many times per session."
```

---

### Task 5: Collapsed-section summaries as a pure module

Spec §3.6. Every collapsed surface carries a one-line summary of its own
content, so information is present at a glance and expanding is for editing,
not discovery.

**Files:**
- Create: `apps/desktop/src/review/summaries.ts`
- Create: `apps/desktop/src/review/summaries.test.ts`

**Interfaces:**
- Consumes: `Asset`, `ClipTranscript`, `AssetAnalysisView` from `../types`.
- Produces, all from `src/review/summaries.ts`:
  - `transcriptSummary(transcript: ClipTranscript | null): string`
  - `transcriptBadge(transcript: ClipTranscript | null): number | null`
  - `analysisSummary(observation: AssetAnalysisView | null): string`
  - `analysisNeedsAttention(observation: AssetAnalysisView | null): boolean`
  - `fileSummary(asset: Asset): string`
  - `descriptionSummary(asset: Asset): string`
  - `clockSummary(asset: Asset): string`

- [ ] **Step 1: Write the failing test**

Create `apps/desktop/src/review/summaries.test.ts`. Reuse the `asset()` factory
shape from `src/review/order.test.ts:4-40` (copy it in — the two files are
independent):

```ts
import { describe, expect, it } from "vitest";
import type { Asset, ClipTranscript } from "../types";
import {
  analysisNeedsAttention,
  clockSummary,
  descriptionSummary,
  fileSummary,
  transcriptBadge,
  transcriptSummary,
} from "./summaries";

function asset(overrides: Partial<Asset> & { id: string }): Asset {
  return {
    projectId: "project_1",
    path: `/media/${overrides.id}.mp4`,
    filename: `${overrides.id}.mp4`,
    fingerprint: `fp_${overrides.id}`,
    sizeBytes: 2_100_000_000,
    durationSeconds: 107,
    frameRate: 59.94,
    width: 3840,
    height: 2160,
    codec: "hevc",
    camera: "Sony A7IV",
    capturedAtRaw: null,
    bestTime: null,
    offsetSeconds: 0,
    proxyPath: null,
    proxyRepresentation: null,
    metadataRotationDegrees: 0,
    defaultRotationDegrees: 0,
    rotationDegrees: 0,
    orientationProbed: true,
    originalAvailable: true,
    proxyAvailable: false,
    thumbnailPath: null,
    thumbnailQuality: 0,
    notes: "",
    title: "",
    description: "",
    labels: [],
    removed: false,
    mediaObjectId: null,
    mediaVerificationState: "unlinked",
    available: true,
    ...overrides,
  };
}

function transcript(overrides: Partial<ClipTranscript> = {}): ClipTranscript {
  return {
    assetId: "a",
    status: "speech",
    primaryLanguage: "en",
    rejectReason: null,
    segments: [
      { startMs: 4000, endMs: 6000, text: "we finally found the place" },
      { startMs: 9000, endMs: 11000, text: "it's like six seats total" },
    ],
    ...overrides,
  };
}

describe("transcriptSummary", () => {
  it("names the segment count and language when there is speech", () => {
    expect(transcriptSummary(transcript())).toBe("Speech · 2 segments · en");
  });

  it("says so plainly when there is no speech, which is half a real corpus", () => {
    expect(transcriptSummary(transcript({ status: "no_speech", segments: [] }))).toBe("No speech detected");
  });

  it("surfaces the reject reason when one exists", () => {
    expect(
      transcriptSummary(transcript({ status: "rejected", segments: [], rejectReason: "music only" })),
    ).toBe("Discarded as unreliable · music only");
  });

  it("reports a failure without inventing a reason", () => {
    expect(transcriptSummary(transcript({ status: "failed", segments: [], rejectReason: null }))).toBe("Transcription failed");
  });

  it("says nothing has run when there is no transcript at all", () => {
    expect(transcriptSummary(null)).toBe("Not transcribed");
  });

  it("uses the singular for one segment", () => {
    expect(transcriptSummary(transcript({ segments: [{ startMs: 0, endMs: 1, text: "hi" }] })))
      .toBe("Speech · 1 segment · en");
  });

  it("omits an unknown language rather than printing a placeholder", () => {
    expect(transcriptSummary(transcript({ primaryLanguage: "" }))).toBe("Speech · 2 segments");
  });
});

describe("transcriptBadge", () => {
  it("is the segment count when there are segments, and null otherwise", () => {
    expect(transcriptBadge(transcript())).toBe(2);
    expect(transcriptBadge(transcript({ segments: [] }))).toBeNull();
    expect(transcriptBadge(null)).toBeNull();
  });
});

describe("fileSummary", () => {
  it("reads camera, codec and size", () => {
    expect(fileSummary(asset({ id: "a" }))).toBe("Sony A7IV · HEVC · 2.1 GB");
  });

  it("uses megabytes below a gigabyte", () => {
    expect(fileSummary(asset({ id: "a", sizeBytes: 420_000_000 }))).toBe("Sony A7IV · HEVC · 420 MB");
  });
});

describe("descriptionSummary", () => {
  it("is the description's first line, trimmed", () => {
    expect(descriptionSummary(asset({ id: "a", description: "  Wide shot of the harbour.\nMore. " })))
      .toBe("Wide shot of the harbour.");
  });

  it("says the field is empty rather than rendering nothing", () => {
    expect(descriptionSummary(asset({ id: "a", description: "   " }))).toBe("No description");
  });
});

describe("clockSummary", () => {
  it("reports no offset when the clock is untouched", () => {
    expect(clockSummary(asset({ id: "a", offsetSeconds: 0 }))).toBe("No offset");
  });

  it("signs the offset in minutes", () => {
    expect(clockSummary(asset({ id: "a", offsetSeconds: 720 }))).toBe("+12 min");
    expect(clockSummary(asset({ id: "a", offsetSeconds: -3000 }))).toBe("−50 min");
  });
});

describe("analysisNeedsAttention", () => {
  it("is true when the clip is flagged unusable or the analysis is stale", () => {
    expect(analysisNeedsAttention(null)).toBe(false);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test summaries`
Expected: FAIL — `Cannot find module './summaries'`.

- [ ] **Step 3: Write the implementation**

Create `apps/desktop/src/review/summaries.ts`. Read
`apps/desktop/src/types.ts` first to confirm the exact shape of
`ClipTranscript` and `AssetAnalysisView`; the `analysisSummary` and
`analysisNeedsAttention` bodies depend on the real field names
(`analysis.usable`, `analysis.unusableReason`, `analysis.cameraMovement`,
`current`). Reuse the existing `transcriptStatusLabels` map from
`App.tsx:52-58` by moving it into this module and importing it back into
`App.tsx`, so the strings have one home.

Signatures and the behaviour each test above pins:

```ts
export function transcriptSummary(transcript: ClipTranscript | null): string;
export function transcriptBadge(transcript: ClipTranscript | null): number | null;
export function analysisSummary(observation: AssetAnalysisView | null): string;
export function analysisNeedsAttention(observation: AssetAnalysisView | null): boolean;
export function fileSummary(asset: Asset): string;
export function descriptionSummary(asset: Asset): string;
export function clockSummary(asset: Asset): string;
```

`fileSummary` reuses the byte formatting rule already in `App.tsx:78-81`
(megabytes below 1e9, one decimal gigabyte above). Move `formatBytes` here too
and import it back into `App.tsx`. `clockSummary` uses U+2212 MINUS SIGN for
negatives, matching the `−15` transport label.

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm --dir apps/desktop test summaries`
Expected: PASS, 16 tests.

- [ ] **Step 5: Run the full suite**

Run: `pnpm --dir apps/desktop test && pnpm --dir apps/desktop check`
Expected: all pass; `App.tsx` compiles against the moved helpers.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/review/summaries.ts apps/desktop/src/review/summaries.test.ts apps/desktop/src/App.tsx
git commit -m "feat(desktop): one-line summaries for collapsed surfaces"
```

---

### Task 6: Shell restructure — three rows, no status line

Spec §1.1, §3.1, §3.2. Fixes the implicit fourth grid row and deletes the
permanently reserved status line.

**Files:**
- Modify: `apps/desktop/src/App.tsx` (`.app-shell` render 1318-1422; the
  chronology drawer 2644-2680; `setMessage` at 319)
- Modify: `apps/desktop/src/styles.css` (`.app-shell` 39; `.status-line`
  91-114; `.workbench` 116)
- Create: `apps/desktop/src/components/__tests__/shell.test.tsx`

**Interfaces:**
- Consumes: `INITIAL_VIEW` and `reduceView` from Task 3.
- Produces: `.app-shell` has exactly three element children in DOM order —
  `header.topbar`, `div.workbench`, `footer.story-ribbon`.

- [ ] **Step 1: Write the failing test**

Create `apps/desktop/src/components/__tests__/shell.test.tsx`:

```tsx
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import App from "../../App";

describe("app shell", () => {
  it("renders exactly three grid children, so no region can take an implicit row", async () => {
    const { container } = render(<App />);
    const shell = container.querySelector(".app-shell");
    expect(shell).not.toBeNull();
    expect(shell!.children).toHaveLength(3);
    expect(shell!.children[0].tagName).toBe("HEADER");
    expect(shell!.children[1]).toHaveClass("workbench");
    expect(shell!.children[2]).toHaveClass("story-ribbon");
  });

  it("has no permanently reserved status line", () => {
    const { container } = render(<App />);
    expect(container.querySelector(".status-line")).toBeNull();
  });

  it("keeps the operation slot empty while nothing is running", () => {
    render(<App />);
    const slot = screen.getByTestId("operation-slot");
    expect(slot).toBeEmptyDOMElement();
  });
});
```

`api.isDesktop()` returns false under jsdom (`api.ts` checks
`__TAURI_INTERNALS__`), so `App` renders against the in-memory mock projects
and assets. Confirm this by reading `apps/desktop/src/api.ts` before writing
the test; if the mock needs a tick to resolve, wrap assertions in
`await screen.findBy…` rather than adding sleeps.

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test shell`
Expected: FAIL — four children, `.status-line` present, no operation slot.

- [ ] **Step 3: Restructure the shell**

In `styles.css:39`:

```css
.app-shell { height: 100vh; display: grid; grid-template-rows: 44px minmax(0, 1fr) auto; }
```

Delete the `.status-line`, `.status-dot`, `.pulse` sizing and
`.operation-progress` grid rules (`styles.css:91-114`), replacing them with the
topbar slot styles:

```css
.operation-slot { min-width: 0; display: flex; align-items: center; gap: 8px; color: var(--muted); font: var(--t-meta)/var(--lh-meta) ui-monospace, SFMono-Regular, Menlo, monospace; white-space: nowrap; overflow: hidden; }
.operation-slot:empty { display: none; }
.operation-slot .progress-track { width: 160px; height: 3px; }
.completion-hairline { height: 2px; background: var(--line); }
.completion-hairline > span { display: block; height: 100%; background: var(--amber); transition: width 240ms ease-out; }
.toast { color: var(--muted); }
.error-banner { color: var(--red); background: rgba(230,120,104,.1); border: 1px solid rgba(230,120,104,.3); border-radius: 3px; padding: 2px 8px; }
```

In `App.tsx`, delete the `<section className="status-line">` block
(1387-1422). Move its three states into a `<div className="operation-slot"
data-testid="operation-slot">` placed in the topbar between the mode switcher
and the right-hand actions. Render the progress readout when
`operationProgress` is non-null, the error banner when `error` is set, and the
toast when `message` is set — and only then. The slot renders nothing at idle.

Delete the idle `setMessage(\`${result.length} clips ready\`)` at `App.tsx:319`,
leaving `setMessage("Choose a media folder to begin")` for the empty case.
Make `message` transient: add an effect clearing it 4s after it changes.

Move the chronology drawer (2644-2680) inside `.workbench` for now, so the
shell has three children; Task 9 removes it entirely. Add
`<footer className="story-ribbon">` as the third child wrapping the existing
`.chronology` markup, moved out of `.viewer-panel`.

Add the completion hairline as the last element inside `header.topbar`,
absolutely positioned at its bottom edge:

```tsx
<div className="completion-hairline" aria-hidden="true">
  <span style={{ width: `${activeAssets.length ? (labeledCount / activeAssets.length) * 100 : 0}%` }} />
</div>
```

Delete `.session-readout` from the topbar and its CSS rule (`styles.css:66`)
plus its `@media` hide (`styles.css:421`).

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm --dir apps/desktop test shell`
Expected: PASS, 3 tests.

- [ ] **Step 5: Verify in the real app**

Run: `pnpm --dir apps/desktop dev --host 127.0.0.1`, open
`http://127.0.0.1:1420/`, and confirm the topbar slot is empty at idle, the
ribbon spans the full width at the bottom, and nothing overflows at 1440x900.
This is the browser mock, so it validates layout only — not media or Tauri
calls.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/App.tsx apps/desktop/src/styles.css apps/desktop/src/components/__tests__/shell.test.tsx
git commit -m "fix(desktop): give the shell its correct child count

.app-shell declared three rows and rendered four children, so the
chronology drawer landed in an implicit auto row and took ~68px of
workbench height whenever a project was open, closed or not.

The status line goes with it: a permanently reserved 28px row whose
content was usually a stale '273 clips ready' restating the queue badge."
```

---

### Task 7: The right rail and the evidence shelf

Spec §3.6, §3.7, rules 1 and 3. This is the task that fixes the reported
problem.

**These two halves ship as one commit.** Emptying the rail (steps 1–4) makes
description, transcript, source and clock event unreachable; the shelf
(steps 5–8) is what gives them back. Splitting them would leave a commit in
which the app is missing fields the user relies on, and this is a daily tool
that may be pulled mid-plan.

**Files:**
- Create: `apps/desktop/src/components/JudgmentPanel.tsx`
- Create: `apps/desktop/src/components/EvidenceShelf.tsx`
- Create: `apps/desktop/src/components/__tests__/JudgmentPanel.test.tsx`
- Create: `apps/desktop/src/components/__tests__/EvidenceShelf.test.tsx`
- Modify: `apps/desktop/src/App.tsx` (inspector render 1789-2015)
- Modify: `apps/desktop/src/styles.css` (`.inspector-*` 256-323)

**Interfaces:**
- Consumes: `Asset`, `AssetAnalysisView` from `../types`.
- Produces: `JudgmentPanel` with props
  `{ asset: Asset; observation: AssetAnalysisView | null; labelDraft: string;
  onLabelDraftChange(value: string): void; onAddLabel(value: string): void;
  onRemoveLabel(label: string): void; onPromoteAiLabel(label: string): void;
  onChangeAsset(next: Asset): void; onCommitAsset(): void;
  onToggleRemoved(): void; saveState: "idle" | "saving" | "saved" }`.

- [ ] **Step 1: Write the failing test**

Create `apps/desktop/src/components/__tests__/JudgmentPanel.test.tsx` with a
local `asset()` factory copied from `src/review/order.test.ts:4-40`, then:

```tsx
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { JudgmentPanel } from "../JudgmentPanel";

// ... asset() factory here ...

function renderPanel(overrides = {}) {
  const props = {
    asset: asset({ id: "a", title: "Ramen alley night walk", labels: ["ramen", "night"] }),
    observation: null,
    labelDraft: "",
    onLabelDraftChange: vi.fn(),
    onAddLabel: vi.fn(),
    onRemoveLabel: vi.fn(),
    onPromoteAiLabel: vi.fn(),
    onChangeAsset: vi.fn(),
    onCommitAsset: vi.fn(),
    onToggleRemoved: vi.fn(),
    saveState: "idle" as const,
    ...overrides,
  };
  return { props, ...render(<JudgmentPanel {...props} />) };
}

describe("rule 1: the rail holds human-authored content only", () => {
  it("renders no transcript, description, or source-metadata surface", () => {
    const { container } = renderPanel();
    expect(container.querySelector(".transcript-section")).toBeNull();
    expect(container.querySelector(".observation-section")).toBeNull();
    expect(container.querySelector(".metadata-grid")).toBeNull();
    expect(container.querySelector(".calibration-section")).toBeNull();
    expect(screen.queryByText(/Raw time|Best time|Transcript|Clock event/i)).toBeNull();
  });

  it("permits the two stated exceptions and nothing else", () => {
    const { container } = renderPanel({
      observation: { current: true, aiLabels: ["alley", "neon"], analysis: { usable: false, unusableReason: "severe shake" } },
    });
    expect(container.querySelectorAll(".ai-suggestion").length).toBe(2);
    expect(container.querySelector(".unusable-chip")).not.toBeNull();
  });
});

describe("rule 3: notes geometry", () => {
  it("renders the notes field before the labels field in DOM order", () => {
    const { container } = renderPanel();
    const nodes = [...container.querySelectorAll("#notes, #label-input")];
    expect(nodes.map((n) => n.id)).toEqual(["notes", "label-input"]);
  });

  it("gives the notes textarea a fixed height rather than a flexible one", () => {
    const { container } = renderPanel();
    const notes = container.querySelector<HTMLTextAreaElement>("#notes")!;
    expect(notes.style.flex).toBe("");
    expect(notes.className).toContain("notes-field");
  });
});

describe("label entry", () => {
  it("has no separate Add button, because Enter and comma already commit", () => {
    renderPanel();
    expect(screen.queryByRole("button", { name: /^add$/i })).toBeNull();
  });

  it("commits on Enter", async () => {
    const { props } = renderPanel({ labelDraft: "osaka" });
    await userEvent.type(screen.getByLabelText(/add label/i), "{Enter}");
    expect(props.onAddLabel).toHaveBeenCalledWith("osaka");
  });

  it("promotes an AI suggestion to a human label on click", async () => {
    const { props } = renderPanel({
      observation: { current: true, aiLabels: ["alley"], analysis: { usable: true, unusableReason: null } },
    });
    await userEvent.click(screen.getByRole("button", { name: /alley/ }));
    expect(props.onPromoteAiLabel).toHaveBeenCalledWith("alley");
  });
});

describe("removal", () => {
  it("offers removal from the header, not a dedicated section", async () => {
    const { props, container } = renderPanel();
    expect(container.querySelector(".remove-section")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: /remove from labeling/i }));
    expect(props.onToggleRemoved).toHaveBeenCalled();
  });
});
```

Adjust the `observation` fixtures to the real `AssetAnalysisView` shape after
reading `src/types.ts`; the assertions, not the fixture literals, are the
contract.

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test JudgmentPanel`
Expected: FAIL — `Cannot find module '../JudgmentPanel'`.

- [ ] **Step 3: Build the component**

Create `JudgmentPanel.tsx` with the §3.7 stack: clip header (index, title
input, save state, `⚠` chip, `⌄` menu with Remove / Reset title / Copy path),
`NOTES` micro-header with save state, the notes textarea, `LABELS`
micro-header, the single-line label input, the chip well, and the AI
suggestion strip.

Carry over from `App.tsx:1813-1839` the label input's existing key handling
(Enter and comma commit, Backspace on empty removes the last chip) and from
`App.tsx:1956-1975` the notes textarea's focus/blur/keydown wiring — but with
the §8 key change: `Enter` inserts a newline, `⌘Enter` saves, `⇧Enter` saves
and advances. Delete the four-shortcut hint strip; it moves to the `?` modal
in Task 11.

CSS, replacing `.inspector-*` and adding the panel:

```css
.judgment-panel { width: 400px; height: 100%; display: grid; grid-template-rows: 32px 18px 400px 18px 28px 120px 30px; align-content: start; gap: 8px 0; padding: 12px 12px 16px; background: var(--panel); border-left: 1px solid var(--line); overflow: hidden; }
.judgment-header { display: flex; align-items: baseline; gap: 6px; min-width: 0; }
.clip-title-field { min-width: 0; flex: 1; padding: 0; color: var(--ink); background: transparent; border: 0; font: 500 var(--t-title)/var(--lh-title) inherit; }
.micro-header { display: flex; align-items: baseline; justify-content: space-between; color: var(--muted); font: 600 var(--t-micro)/var(--lh-micro) ui-monospace, SFMono-Regular, Menlo, monospace; letter-spacing: .06em; text-transform: uppercase; }
.notes-field { width: 100%; height: 400px; resize: none; padding: 12px; color: var(--ink); background: var(--graphite); border: 1px solid var(--line); border-radius: 3px; font: var(--t-notes)/var(--lh-notes) inherit; }
.notes-field:focus { outline: none; border-color: var(--amber); }
.label-field { width: 100%; height: 28px; padding: 0 8px; color: var(--ink); background: var(--graphite); border: 1px solid var(--line); border-radius: 3px; font: var(--t-body)/var(--lh-body) inherit; }
.chip-well { height: 120px; display: flex; flex-wrap: wrap; align-content: start; gap: 4px; overflow-y: auto; }
.ai-strip { display: flex; flex-wrap: wrap; align-items: center; gap: 4px; height: 30px; overflow: hidden; }
.ai-suggestion { padding: 2px 8px; color: var(--cyan); background: transparent; border: 1px dashed #2c4c52; border-radius: 3px; cursor: pointer; font: var(--t-chip)/var(--lh-chip) inherit; }
```

The grid rows are fixed and sum to 646 plus 6 gaps of 8 plus 28 of padding =
702; set `align-content: start` so the panel does not stretch and rule 3 holds
when the main row is taller than 700 (spec §3.1).

In `App.tsx`, replace the `<aside className="inspector-panel">` block
(1789-2015) with `<JudgmentPanel … />`. Wire `onPromoteAiLabel` to the existing
`addLabel(selected, label)`.

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm --dir apps/desktop test JudgmentPanel`
Expected: PASS, 8 tests.

- [ ] **Step 5: Write the failing shelf test**

The rail is now empty of machine output; the shelf is what restores it.

**Shelf interfaces:**
- Consumes: `transcriptSummary`, `transcriptBadge`, `analysisSummary`,
  `analysisNeedsAttention`, `fileSummary` from Task 5;
  `reduceView`/`ViewState` from Task 3.
- Produces: `EvidenceShelf` with props
  `{ asset: Asset; transcript: ClipTranscript | null;
  observation: AssetAnalysisView | null; open: boolean; tab: ShelfTab;
  playbackSeconds: number; onTabChange(tab: ShelfTab): void;
  onToggleOpen(): void; onSeek(seconds: number): void;
  onChangeAsset(next: Asset): void; onCommitAsset(): void }`, and
  `type ShelfTab = "transcript" | "description" | "analysis" | "file"`.

Create `apps/desktop/src/components/__tests__/EvidenceShelf.test.tsx`
asserting:

```tsx
describe("collapsed state", () => {
  it("shows the tab strip with badges but no pane content when closed", () => {
    // renders 4 tabs; transcript tab shows its segment count; no <ol> present
  });

  it("marks the analysis tab when the analysis is stale or the clip is unusable", () => {
    // .shelf-tab[data-attention="true"] on the analysis tab
  });
});

describe("transcript pane", () => {
  it("seeks to a segment's start when its row is clicked", async () => {
    // click the "0:04" row, expect onSeek toHaveBeenCalledWith(4)
  });

  it("states the reason rather than rendering an empty list when there is no speech", () => {
    // status no_speech -> "No speech in this clip." and no listitem
  });
});

describe("tab persistence", () => {
  it("reports a tab change without owning the state", async () => {
    // click File tab, expect onTabChange toHaveBeenCalledWith("file")
  });
});
```

Write these out fully with the same `asset()`/`transcript()` factories used in
Task 5's test file.

- [ ] **Step 6: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test EvidenceShelf`
Expected: FAIL — module not found.

- [ ] **Step 7: Build the shelf**

Port the transcript list from `App.tsx:1926-1954`, the description editor and
analysis facts from `App.tsx:1882-1924`, and the source grid from
`App.tsx:1988-1994` into the four panes. The File pane additionally shows the
camera-clock/corrected-time pair with its delta, and the Clock event form from
`App.tsx:1996-2010` moves here as the File pane's footer.

CSS:

```css
.evidence-shelf { min-width: 0; display: grid; grid-template-rows: 28px minmax(0, auto); background: var(--panel); border-top: 1px solid var(--line); }
.shelf-tabs { display: flex; align-items: stretch; gap: 0; padding: 0 12px; }
.shelf-tab { padding: 0 12px; color: var(--muted); background: transparent; border: 0; border-bottom: 2px solid transparent; cursor: pointer; font: var(--t-body)/var(--lh-body) inherit; }
.shelf-tab[aria-selected="true"] { color: var(--ink); border-bottom-color: var(--amber); }
.shelf-tab[data-attention="true"]::after { content: ""; display: inline-block; width: 5px; height: 5px; margin-left: 6px; background: var(--amber); border-radius: 50%; }
.shelf-pane { height: 246px; padding: 12px; overflow-y: auto; }
.shelf-pane .segment { display: grid; grid-template-columns: 44px minmax(0, 1fr); align-items: baseline; gap: 8px; width: 100%; height: 22px; padding: 0; color: var(--ink); background: transparent; border: 0; cursor: pointer; text-align: left; font: var(--t-body)/var(--lh-body) inherit; }
.shelf-pane .segment:hover { color: var(--amber); }
.shelf-pane .segment time { color: var(--quiet); font: var(--t-meta)/var(--lh-meta) ui-monospace, SFMono-Regular, Menlo, monospace; }
```

Note the tab strip and the pane are separate rows so the strip stays visible
when `open` is false; render the pane only when `open`.

In `App.tsx`, place `<EvidenceShelf />` as the third row of `.viewer-panel` and
hold `shelfTab` in state so it persists across clip changes (spec §3.6: it is a
mode, not per-clip state). Wire `D` from the Task 4 keymap to `toggleShelf`.

- [ ] **Step 8: Run both test files and confirm nothing was lost**

Run: `pnpm --dir apps/desktop test JudgmentPanel EvidenceShelf`
Expected: PASS. The rule-1 test must still pass with the shelf present.

Run `pnpm --dir apps/desktop test && pnpm --dir apps/desktop check`, then check
by hand in `pnpm --dir apps/desktop dev` that description, transcript, camera,
codec, size, raw/best time and the clock-event form are all reachable again.
Nothing the rail used to show may be missing from the app at this commit.

- [ ] **Step 9: Commit**

```bash
git add apps/desktop/src/components/JudgmentPanel.tsx apps/desktop/src/components/EvidenceShelf.tsx apps/desktop/src/components/__tests__ apps/desktop/src/App.tsx apps/desktop/src/styles.css
git commit -m "feat(desktop): human-only rail, machine output under the player

The inspector held ~1387px of content in a ~750px viewport, so with a
transcript present the notes field began 81px below the fold and was never
visible without scrolling.

The rail now holds only what a human authors, with two stated exceptions,
and a test enforces that rather than leaving it to review. Everything it
shed lands in a shelf beneath the player: transcript segments are seek
controls and belong beside the transport, and at 740px a segment fits one
line where at 400px it wrapped to three. The shelf is collapsed by
default, so the video stage keeps 640px."
```

---

### Task 8: VideoStage and the permanent TransportBar

Spec §3.4, §3.5. Deletes the auto-hiding overlay and the duplicate bar.

**Files:**
- Create: `apps/desktop/src/components/VideoStage.tsx`
- Create: `apps/desktop/src/components/TransportBar.tsx`
- Create: `apps/desktop/src/components/__tests__/TransportBar.test.tsx`
- Modify: `apps/desktop/src/App.tsx` (viewer 1515-1670)
- Modify: `apps/desktop/src/styles.css` (`.viewer-*` 177-212)

**Interfaces:**
- Consumes: `Asset` from `../types`.
- Produces: `TransportBar` with props
  `{ asset: Asset; playing: boolean; muted: boolean; positionSeconds: number;
  durationSeconds: number; bestMomentMs: number | null;
  segmentStartsMs: number[]; onTogglePlay(): void; onSeek(seconds: number): void;
  onSeekBy(delta: number): void; onToggleMute(): void; onRotate(): void;
  onEnterFocus(): void; onStep(delta: number): void }`.

- [ ] **Step 1: Write the failing test**

```tsx
describe("transport bar", () => {
  it("is always rendered, never conditional on hover or playback", () => {
    const { rerender, container } = render(<TransportBar {...props} playing={false} />);
    expect(container.querySelector(".transport")).not.toBeNull();
    rerender(<TransportBar {...props} playing={true} />);
    expect(container.querySelector(".transport")).not.toBeNull();
  });

  it("marks the best moment on the scrubber and jumps to it on click", async () => {
    // .best-moment-marker present at the right offset; click -> onSeek(42)
  });

  it("renders one tick per transcript segment start", () => {
    // segmentStartsMs of length 3 -> 3 .segment-tick nodes
  });

  it("omits the best-moment marker when the analysis found none", () => {
    // bestMomentMs null -> no .best-moment-marker
  });

  it("enters Focus from the expand control", async () => {
    // click "Fullscreen" -> onEnterFocus called
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test TransportBar`
Expected: FAIL — module not found.

- [ ] **Step 3: Build the components**

`VideoStage` takes the `bufferedMedia` render from `App.tsx:1525-1585` plus the
placeholder slates, the `.viewer-overlay` PROXY/ORIGINAL readout, and the
removed-clip strip. Buffering becomes a 2px hairline, replacing the
`.viewer-buffering` pill.

`TransportBar` carries play/pause, −15, +15, the time readout, the scrubber
with markers, mute, the rotation glyph and popover, the PROXY/ORIGINAL badge,
previous/next, and the expand control.

Delete `.viewer-controls`, its auto-hide rule (`styles.css:189`),
`.viewer-stage:fullscreen` (`styles.css:201-202`), `.transport-bar` and
`.transport-*` (`styles.css:204-212`). The rotation section
(`styles.css:277-294`) collapses to the glyph and popover.

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm --dir apps/desktop test TransportBar`
Expected: PASS.

- [ ] **Step 5: Verify playback in the real app**

Run the Tauri app (`pnpm --dir apps/desktop tauri dev`) against a project with
real media. The browser mock cannot validate asset-protocol media URLs. Confirm
play/pause, seek, ±15, mute, rotation, and that the bar never disappears during
playback.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/components/VideoStage.tsx apps/desktop/src/components/TransportBar.tsx apps/desktop/src/components/__tests__/TransportBar.test.tsx apps/desktop/src/App.tsx apps/desktop/src/styles.css
git commit -m "feat(desktop): one permanent transport bar

The 46px transport bar duplicated play, timecode and mute from the overlay,
and the overlay hid itself during playback -- a scrubber you cannot aim at
while watching is the one time you want it. One bar replaces both, and the
best moment becomes a scrubber marker instead of a button printing a
timestamp as text."
```

---

### Task 9: StoryRibbon full-width, and modes

Spec §3.8, §6. Moves chronology and edit planning out of the drawer.

**Files:**
- Create: `apps/desktop/src/components/StoryRibbon.tsx`
- Create: `apps/desktop/src/components/__tests__/StoryRibbon.test.tsx`
- Modify: `apps/desktop/src/App.tsx` (chronology 1672-1786; drawer 2644-2680)
- Modify: `apps/desktop/src/styles.css` (`.chronology*`, `.story-*` 214-254)

**Interfaces:**
- Consumes: `ribbonHeight`, `ribbonLaneRows`, `visibleLaneCount` from Task 2;
  `reduceView` from Task 3.
- Produces: `StoryRibbon` with props
  `{ lanes: Array<[string, Asset[]]>; assets: Asset[]; selectedId: string;
  zoom: number; collapsed: boolean; merged: boolean; contextRuns: ContextRun[];
  onSelect(id: string): void; onZoom(delta: number): void; onCenter(): void;
  onToggleCollapsed(): void }`. `merged` renders the single-lane variant Focus
  uses (Task 10).

- [ ] **Step 1: Write the failing test**

```tsx
describe("adaptive lanes", () => {
  it("emits one lane track per camera rather than a hardcoded five", () => {
    // 2 lanes -> canvas style gridTemplateRows contains "repeat(2, 20px)"
  });

  it("renders a sixth camera rather than clipping it", () => {
    // 6 lanes -> 6 .lane-track nodes present
  });

  it("collapses to the merged single lane when merged is set", () => {
    // merged -> exactly 1 .lane-track, and no per-camera gutter names
  });
});

describe("selection", () => {
  it("selects a clip when its block is clicked", async () => {
    // click block -> onSelect(id)
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test StoryRibbon`
Expected: FAIL — module not found.

- [ ] **Step 3: Build the component and the modes**

Port `App.tsx:1672-1786` into `StoryRibbon`, replacing both hardcoded
`repeat(5, 22px)` declarations with `ribbonLaneRows(lanes.length)` applied as
an inline style, and setting the ribbon's own height from
`ribbonHeight(lanes.length, collapsed)`. Collapse the 45px heading to the 22px
control row and drop the `N clips in relative order` line.

Add mode routing in `App.tsx`: when `view.mode !== "review"`, render
`<CalibrationPanel />` or `<EditPlanPanel />` full-window in place of
`.workbench` and the ribbon. Delete the `.chronology-drawer` block entirely and
its CSS (`styles.css:582-598`). Add the segmented control to the topbar and
wire the Task 4 `setMode` action. Preserve on return: `selectedId`, queue
scroll, `chronologyZoom`, `shelfTab`.

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm --dir apps/desktop test StoryRibbon`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add apps/desktop/src/components/StoryRibbon.tsx apps/desktop/src/components/__tests__/StoryRibbon.test.tsx apps/desktop/src/App.tsx apps/desktop/src/styles.css
git commit -m "feat(desktop): full-width ribbon and first-class modes

The ribbon lived inside the viewer column, so it cost the video 244px and
saw only 863px of width. Full width it shows ~21 clips instead of ~12, and
the video column stops paying for it.

Chronology and edit planning become modes rather than a 45vh drawer: they
are global, sit-down operations that were being asked to work in a strip
at the bottom of the review screen."
```

---

### Task 10: The Focus presentation

Spec §3.9.

**Files:**
- Create: `apps/desktop/src/components/FocusView.tsx`
- Create: `apps/desktop/src/components/__tests__/FocusView.test.tsx`
- Modify: `apps/desktop/src/App.tsx`
- Modify: `apps/desktop/src/styles.css`

**Interfaces:**
- Consumes: `ViewState`, `reduceView`, `focusesNotesOnEntry` from Task 3;
  `JudgmentPanel`'s notes and label subcomponents from Task 7;
  `StoryRibbon` with `merged` from Task 9.
- Produces: `FocusView` with props
  `{ asset: Asset; view: ViewState; transcript: ClipTranscript | null; … }`
  mirroring the Review handlers.

- [ ] **Step 1: Write the failing test**

```tsx
describe("focus overlays", () => {
  it("hides the right stack by default", () => { /* .focus-right-stack absent */ });
  it("renders notes, ribbon and transport by default", () => { /* three present */ });
  it("hides every overlay when overlays is false", () => { /* none present, video still rendered */ });
  it("never changes overlay visibility on pointer movement", async () => {
    // fire pointerenter/pointerleave/mousemove on the root; overlay set unchanged
  });
});

describe("caret preservation", () => {
  it("restores the caret offset it was given", () => {
    // render with notesCaret=7, expect the textarea's selectionStart to be 7
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test FocusView`
Expected: FAIL — module not found.

- [ ] **Step 3: Build the component**

Layout per §3.9: video full-bleed `object-fit: contain`; top strip 1440x36;
notes card 600x420 anchored 24 from the left and 16 above the bottom stack;
right stack 360x760 at 24 from the right; transport 1440x28; ribbon overlay
1440x44 in `merged` mode. Cards use
`background: rgba(23,28,32,.86); backdrop-filter: blur(8px)`, matching the
existing `.inspector-title` treatment.

In `App.tsx`, hold `notesCaret` in a ref, capture it on presentation change,
and restore it after the new textarea mounts. Wire `F`, `H`, `D`, `T` and Esc
through the Task 4 actions and the Task 3 reducer. Esc must close any open
modal first and only then leave Focus.

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm --dir apps/desktop test FocusView`
Expected: PASS.

- [ ] **Step 5: Verify the round trip by hand**

In `pnpm --dir apps/desktop tauri dev`: type mid-sentence in notes, press `F`,
type more, press `F` again, and confirm the caret lands on the same character
and the video neither restarted nor changed play state.

- [ ] **Step 6: Commit**

```bash
git add apps/desktop/src/components/FocusView.tsx apps/desktop/src/components/__tests__/FocusView.test.tsx apps/desktop/src/App.tsx apps/desktop/src/styles.css
git commit -m "feat(desktop): the Focus notes-writing presentation

Writing a note is the one task where the three-column layout works against
you. Focus fills the window with the frame and floats only the writing
surface, a merged-lane ribbon and a minimal transport.

It subsumes fullscreen: the expand control enters Focus with overlays
hidden, so there is one fullscreen concept rather than two. Native macOS
fullscreen is deliberately not used -- animating to a separate Space over
~700ms defeats a toggle meant to be pressed dozens of times a session."
```

---

### Task 11: Density pass and final verification

Spec §5, §10. Applies the type and spacing scales everywhere and closes out.

**Files:**
- Modify: `apps/desktop/src/styles.css` (whole file)
- Modify: `apps/desktop/src/App.tsx` (shortcuts modal, media menu hint)
- Create: `apps/desktop/src/review/scale.test.ts`

- [ ] **Step 1: Write the failing test**

Create `apps/desktop/src/review/scale.test.ts` asserting the type scale is
respected, reusing the Task 1 file reader:

```ts
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const css = readFileSync(fileURLToPath(new URL("../styles.css", import.meta.url)), "utf8");
const ALLOWED = new Set([10, 11, 12, 13, 15, 17, 21]);

describe("type scale", () => {
  it("uses no font size outside the scale", () => {
    const sizes = [...css.matchAll(/font-size:\s*(\d+)px/g)].map((m) => Number(m[1]));
    const shorthand = [...css.matchAll(/font:\s*(?:\d+\s+)?(\d+)px/g)].map((m) => Number(m[1]));
    const offenders = [...new Set([...sizes, ...shorthand])].filter((size) => !ALLOWED.has(size));
    expect(offenders.sort((a, b) => a - b)).toEqual([]);
  });

  it("uses 15px for the notes field and nothing else", () => {
    const rules = css.split("}").filter((rule) => /(?:font-size:\s*15px|font:[^;]*\b15px)/.test(rule));
    expect(rules).toHaveLength(1);
    expect(rules[0]).toContain(".notes-field");
  });
});
```

21px is in the allowed set for the modal `h2` headings, which are outside the
main-view scale.

- [ ] **Step 2: Run test to verify it fails**

Run: `pnpm --dir apps/desktop test scale`
Expected: FAIL, listing every 8px and 9px rule still present.

- [ ] **Step 3: Apply the scale**

Sweep `styles.css`: every 8px and 9px font size becomes 10px or 11px per §5;
`.queue-item` becomes 52px tall with a 72x40 thumbnail; `.inspector-section`
padding drops to 9px 12px where such rules remain; spacing values snap to the
2/4/6/8/12/16/24 scale.

Update the shortcuts modal in `App.tsx` with `F`, `H`, `D`, `T`, `⇧T`, `B`,
`I`, `⌘1/2/3` and the moved `⇧G`, and add the four notes shortcuts removed in
Task 7. Change the media menu's thumbnails hint from `<kbd>T</kbd>` to
`<kbd>⇧G</kbd>`.

- [ ] **Step 4: Run test to verify it passes**

Run: `pnpm --dir apps/desktop test scale`
Expected: PASS.

- [ ] **Step 5: Run the full suite and build**

Run: `pnpm --dir apps/desktop test && pnpm --dir apps/desktop check && pnpm --dir apps/desktop build`
Expected: all pass.

- [ ] **Step 6: Work the spec §10 verification list**

Run `pnpm --dir apps/desktop tauri dev` at 1440x900 and 1280x800 and confirm
every bullet in spec §10, including the Focus round trip, the 2-camera and
6-camera ribbon, and that no topbar control shows a mismatched outline. Record
the measured video stage height, ribbon width, and judgment panel scroll
height against the §10 targets.

- [ ] **Step 7: Update AGENTS.md**

AGENTS.md line 103 states "desktop React still has no component runner". That
is now false. Update it to record the component tests, and update the
implemented-slice list to describe the new main view.

- [ ] **Step 8: Commit**

```bash
git add apps/desktop/src/styles.css apps/desktop/src/App.tsx apps/desktop/src/review/scale.test.ts AGENTS.md
git commit -m "feat(desktop): density pass across the main view

The type scale had five steps below 13px, bottoming out at 8px, where
timeline clip names and queue indices read as texture rather than text.
Six sizes now, with 15px reserved for the notes field so the hierarchy is
stated in the scale rather than in chrome."
```

---

## Self-Review

**Spec coverage.** §1.3 and §7 defects → Task 1 (tokens, disabled buttons,
select border, Catppuccin/rem port), Task 6 (implicit row, status line), Task 8
(duplicate transport), Task 9 (hardcoded lanes). §2 rules → Task 3 (rule 4
sequencing), Task 7 (rules 1 and 3, both test-enforced). §3.1 → Task 6. §3.2 →
Task 6. §3.3 → Task 11 (queue row geometry). §3.4/§3.5 → Task 8. §3.6 → Tasks 5
and 7. §3.7 → Task 7. §3.8 → Tasks 2 and 9. §3.9 → Tasks 3 and 10. §4 → the
task that owns each destination. §5 → Task 11. §6 → Task 9. §8 → Task 4. §9 is
out of scope by construction. §10 → Task 11 step 6.

**Gap found and closed:** §3.3's queue row spec (52px, 72x40 thumbnail, flag
column, removed-row hatch) had no owning task; it is now Task 11 step 3. The
queue is otherwise unchanged by this plan, so it does not need its own task.

**Placeholder scan.** Tasks 7 (shelf half), 8, 9 and 10 give test intent as
commented `it()` bodies rather than full assertions. That is deliberate for
tests whose fixtures depend on the real `AssetAnalysisView` and `ContextRun`
shapes in `src/types.ts`, which the implementer must read first; each carries
an explicit instruction to do so. Every other step contains runnable content.

**Type consistency.** `ShelfTab` is defined in Task 7 and consumed by Task 10.
`ViewState`/`ViewAction`/`reduceView` are defined in Task 3 and consumed by
Tasks 6, 7, 9 and 10 under those exact names. `ribbonHeight`/`ribbonLaneRows`/
`visibleLaneCount` are defined in Task 2 and consumed by Task 9.
`transcriptSummary`/`transcriptBadge`/`analysisSummary`/
`analysisNeedsAttention`/`fileSummary`/`descriptionSummary`/`clockSummary` are
defined in Task 5 and consumed by Task 7. `toggleShelf` and `toggleRibbon`
carry the same names in the keymap (Task 4) and the reducer (Task 3).
