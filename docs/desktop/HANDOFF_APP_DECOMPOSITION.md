# Handoff: App.tsx decomposition

**Date:** 2026-08-06
**Status:** The review-layout plan's component extraction is complete. App.tsx is 2173 lines; the modals are what is left in it.
**Branch:** merged to `main`

This records what a follow-on session needs in order to continue, and nothing
else. It is not a summary of work done — the commit messages carry that.

## Where the decomposition stopped

`apps/desktop/src/components/` now holds `TopBar`, `QueuePanel`, `VideoStage`,
`TransportBar`, `EvidenceShelf`, `JudgmentPanel`, `StoryRibbon`, `Splitter`,
each with a test file under `__tests__/`. Pure logic lives in
`apps/desktop/src/review/`: `layout`, `presentation`, `summaries`, `keymap`,
`order`, `panelSizes`, `tokens`, `format`, `rotation`.

`App.tsx` keeps state, effects, handlers, and the shell's three-row tree. What
remains of its render tree is **seven modals, lines 1536–2168** — roughly 630
of the file's 2173 lines:

| Line | Modal | Note |
| --- | --- | --- |
| 1536 | New project / project library | |
| 1585 | Settings | ~140 lines, the largest |
| 1727 | Pairing wizard | multi-step, own Escape handler |
| 1808 | Baseline wizard | multi-step, own Escape handler |
| 1922 | Analysis dialog | gated on `readiness` |
| 2009 | Keyboard shortcuts | |
| 2051 | Sync diagnostics | own Escape handler |

**This is the obvious next extraction and it was never planned for**, so it has
no design behind it. Two things to settle before starting rather than during:

1. The wizards own multi-step local state (`pairingWizardStep`, `pairingCode`,
   `serverIdentity`, and the baseline equivalents) that currently lives in
   `App`. A wizard component that owns its own step state is a different
   design from one that receives it, and the choice should be made once and
   applied to both.
2. Four of the seven handle Escape inline on the backdrop while the global
   keymap in `review/keymap.ts` also resolves Escape against a modal list
   (`App.tsx:646`). That duplication predates this work; extracting modals is
   the moment it either gets unified or gets deliberately left alone.

## Conventions the extracted components follow

Match these or change them everywhere — do not add a third style.

- **Flat props, no grouped objects.** `TopBar` takes 28 and that is accepted.
- **A component owns the type of the data it renders**, and the shell imports
  it: `BufferedMedia` from `VideoStage`, `OperationProgress` from `TopBar`,
  `ContextRun` from `StoryRibbon`, `SaveState` from `JudgmentPanel`.
- **Shared pure helpers go to `src/review/`, never copied into a component.**
  Two rounds of extraction produced hand-copied `formatDuration` and
  `formatTimecode` variants before this was fixed.
- **One test file per component**, with a local `asset()` factory. There is no
  shared fixture module and adding one has not been needed.

## Invariants that tests enforce — do not route around

- **Rule 1: the right rail holds human-authored content only.**
  `JudgmentPanel.test.tsx` asserts the absence of transcript, description,
  metadata, and calibration surfaces. Two exceptions are permitted and both
  are named in the test. This is the rule the whole layout depends on; it is a
  rule rather than an ordering because the previous ordering decayed.
- **The shell renders exactly three children.** A fourth takes an implicit
  grid row — the original bug. `shell.test.tsx` covers it.
- **Every `var()` in styles.css resolves** (`review/tokens.test.ts`) and every
  font size is on the six-step scale (`review/scale.test.ts`).
- **Focus is a CSS presentation over the mounted tree, not a separate
  component.** The 2026-08-04 plan's file list names a `FocusView.tsx`; it was
  deliberately not built, because unmounting the textarea and the video
  element is what would lose the caret and the playback position across the
  transition. Do not "finish" the plan by creating it.

## Known open items

- **`focus.test.tsx` and `shell.test.tsx` emit React `act(...)` warnings.**
  345 tests pass and no assertion is affected, but the warnings are real: both
  suites drive `App` through global key events whose state updates land
  outside `act`. Worth fixing before the suites grow.
- **Spec §10 verification at 1280×800 has no recorded evidence.** The layout
  commits record measurements at 1440×900 only. Whether the 1280×800 pass was
  run and not written down, or not run, cannot be determined from the repo.
  Re-run it rather than assume.
- **No automated native Tauri UI suite.** The component tests are jsdom;
  native playback, filesystem prompts, and sleep/wake remain manual
  (AGENTS.md:116, `docs/desktop/smoke-test.md`).

## Verification gates

```
pnpm --dir apps/desktop test && pnpm --dir apps/desktop check && pnpm --dir apps/desktop build
```

At the time of writing: 21 files, 345 tests, tsc clean, build clean. The CSS
bundle hash was unchanged across the whole decomposition, which is the check
worth repeating — a refactor that moves markup should not move a byte of CSS.

For the running app, `.claude/launch.json` defines `desktop-preview` on port
1420. The browser build reports `api.isDesktop() === false`, so the catalog
and pairing controls are absent there by design.
