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

`App.tsx` keeps state, effects, handlers, and the shell's three-row tree. The
seven modals it used to render inline were **extracted in #20**; each is now a
component in `src/components/` with its own test file:

| Component | Note |
| --- | --- |
| `NewProjectModal` | project library + create |
| `SettingsModal` | the largest; owns the `autoplayOptions` it renders |
| `PairingWizardModal` | three-step, receives its step |
| `BaselineWizardModal` | five-step, receives its step |
| `AnalysisDialog` | gated on `readiness` |
| `KeyboardShortcutsModal` | presentational |
| `SyncDiagnosticsModal` | presentational; Escape now belongs to the shell |

### Decisions settled in #20 (applied to all seven)

1. **Wizards receive their step state; they do not own it.** `pairingWizardStep`,
   `pairingCode`, `serverIdentity`, and the baseline equivalents stay in `App`
   and arrive as flat props. This kept the shell as the single owner of the
   reset each wizard needs, so Cancel and Escape both resolve to one
   `onCancel` callback rather than to state scattered across a component and its
   parent.
2. **Backdrop Escape was left as-is, deliberately** — and has since been
   unified, which is what that deferral was waiting for. Issue #52 moved every
   surface into one ordered stack in `apps/desktop/src/review/escape.ts`; the
   four modals that watched for Escape themselves no longer do, and
   `TripPhaseDialog` keeps only its focus containment. See that module for the
   order and the lock semantics.

Conventions followed: flat props (no grouped objects); a component owns the type
of the data it renders (`SettingsModal` carries `autoplayOptions`,
`PairingWizardModal` exports `PairingWizardStep`, `BaselineWizardModal` exports
`BaselineStep`); shared pure helpers stayed in `src/review/` (`formatCost`,
`formatBytes`, `syncStateLabel`); one test file per component. The CSS bundle
hash was unchanged across the whole extraction.

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

- **~~`focus.test.tsx` and `shell.test.tsx` emit React `act(...)` warnings.~~**
  Resolved (#26). Both suites drive `App` through `userEvent`/`act`-wrapped key
  events; the full desktop suite now emits no `act(...)` warnings.
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
