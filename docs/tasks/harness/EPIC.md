# H01: Fast desktop CUJ test harness

<!-- bounded-epic-metadata
{
  "schema_version": 1,
  "epic_id": "H01",
  "title": "Fast desktop CUJ test harness",
  "base_branch": "main",
  "task_branch_template": "claude/{task_id_lower}-{slug}",
  "tasks_per_cycle": 1,
  "merge_requires_user_authorization": true
}
-->

## Outcome

Make a complete desktop user journey cheap to test. Boot the real `App` in
Vitest, let it call the real `api.ts` and `chronologyApi.ts`, and answer every
one of its Tauri calls from a deterministic in-memory backend that behaves like
a backend: writes are visible to later reads, progress arrives when the test
says so, and failures can be injected on demand.

The epic succeeds when the primary review loop, the analysis loop, the media
generation loop, the chronology correction loop, and the main failure paths are
each covered by full-`App` scenarios that run in seconds with no Chrome, no
Tauri, no FFmpeg, and no network — and when the browser and native suites are
left holding only what genuinely needs a browser or a Mac.

## Why this epic exists

`apps/desktop` has hundreds of unit and component tests and a large native and
Rust suite, and almost nothing between them. Three files render the whole `App`,
and they ran against the `if (!inTauri())` demo branches in `api.ts` — a preview
convenience that reported proxy generation it never performed, returned `null`
for every media URL, returned empty results for the entire chronology surface,
and slept on real timers for 140 ms per clip per stage. (H01-T010 deleted those
branches and put the preview on this harness; the three files now boot against a
declared scenario.)

So the cheap tier cannot express "open a project, review clips, annotate, filter,
come back, and check everything still agrees", and cannot express any failure
path at all. Those journeys are today only covered by manual smoke testing
(`docs/desktop/smoke-test.md`) and by whatever a reviewer happens to click.

This epic adds the missing tier at the Tauri IPC boundary. It does not
reimplement domain logic in TypeScript, and it does not relabel any existing
native, media, or topology gate.

## Frozen context

- Architecture contract: `docs/tasks/harness/ARCHITECTURE.md`
- Production findings this epic recorded: `docs/tasks/harness/FINDINGS.md`
- Testing authority: `docs/integration-testing.md`, `AGENTS.md` §12
- Desktop UI conventions: `AGENTS.md` §10
- Media boundary: `docs/tasks/desktop/MEDIA_DELIVERY.md`
- Chronology authority: `docs/tasks/desktop/CHRONOLOGY_MODEL.md`
- Edit-plan authority: `docs/tasks/desktop/EDIT_PLAN_MODEL.md`

### Architectural constraints

- **The seam is `window.__TAURI_INTERNALS__`.** Tests install a fake there and
  let the real `@tauri-apps/api` reach it. No CUJ may `vi.mock("../api")`,
  `vi.mock("../chronologyApi")`, or otherwise replace an adapter — the command
  names, argument shapes, and event subscriptions are part of what is under
  test.
- **Writes are observable.** A handler that reports work must change canonical
  state such that a later read shows it. Reporting generation that leaves state
  untouched is a harness bug, not an acceptable shortcut.
- **Unknown commands throw.** No permissive default. A new production `invoke()`
  fails the suite until a handler exists.
- **No wall-clock waiting.** Background work resolves when the test releases it.
  Harness code contains no `setTimeout`-based sleeps.
- **No second solver.** Chronology, diagnostics, edit-plan, validation, and OTIO
  responses are scenario-declared outcomes. No TypeScript reimplements a rule
  that lives in `rcdvlog-core`.
- **No fabricated native evidence.** Media in this tier is logical playback
  state driven by real DOM events. Nothing in this epic may be cited as evidence
  for WKWebView frames, decoding, Keychain, dialogs, filesystem permissions,
  FFmpeg, or NAS behavior.
- **Deterministic identity.** No `crypto.randomUUID()`, `new Date()`, or
  `Math.random()` in harness or scenario code.
- **The preview branches stay until T010.** `api.ts`'s `!inTauri()` paths kept
  serving the Vite preview until H01-T010 decided their fate; no other task
  deleted them as a side effect. T010 removed them and installed this harness in
  the preview instead, so the constraint is now: **there is one fake backend**,
  and nothing may add a second.

## Queue

| Order | Task | Title | Depends on | Status |
| ---: | --- | --- | --- | --- |
| 10 | [H01-T001](tasks/H01-T001-harness-foundation-and-ipc-seam.md) | Harness foundation and IPC seam | — | complete |
| 20 | [H01-T002](tasks/H01-T002-review-loop-cujs.md) | Review-loop CUJs | H01-T001 | complete |
| 30 | [H01-T003](tasks/H01-T003-media-state-and-generation-cujs.md) | Media state and generation CUJs | H01-T001 | complete |
| 40 | [H01-T004](tasks/H01-T004-analysis-cujs.md) | Analysis CUJs | H01-T001 | complete |
| 50 | [H01-T005](tasks/H01-T005-chronology-and-edit-plan-cujs.md) | Chronology and edit-plan CUJs | H01-T001 | complete |
| 60 | [H01-T006](tasks/H01-T006-failure-and-recovery-matrix.md) | Failure and recovery matrix | H01-T002, H01-T003 | complete |
| 70 | [H01-T007](tasks/H01-T007-import-and-card-cujs.md) | Import and card CUJs | H01-T003 | complete |
| 80 | [H01-T008](tasks/H01-T008-ipc-contract-coverage-suite.md) | IPC contract coverage suite | H01-T004, H01-T005 | complete |
| 90 | [H01-T009](tasks/H01-T009-ci-wiring-and-documentation.md) | CI wiring and documentation reconciliation | H01-T008 | complete |
| 100 | [H01-T010](tasks/H01-T010-browser-preview-rationalization.md) | Browser-preview rationalization | H01-T009 | complete |

### Execution note

The packets are written to the bounded-task schema so they can be run through
the orchestrator, but this epic is being executed directly on one branch with a
commit per packet. Status therefore moves `ready → complete` on the commit that
lands the packet's tests, without a separate independent-tester cycle; the
independent-test charters remain as the falsification agenda for whoever picks
one up.

### Sequencing rationale

**T001 is the whole risk.** If the IPC seam does not hold — if the real
`@tauri-apps/api` cannot be pointed at a fake, or if `App` cannot boot against
one — every later task is worthless. It ships the smallest thing that proves the
seam: the real `App`, booting, through the real adapters, with every command it
issues at startup answered explicitly and logged.

**T002 through T005 are parallel in principle and serialized in practice.** Each
adds one command family and the journeys that family unlocks. They are ordered by
how often the workflow is used, not by how interesting it is: the review loop is
what the app is for.

**T006 comes after the loops it breaks.** Fault injection is designed in T001,
but a failure matrix is only meaningful once there are happy paths to fail.

**T008 is the ratchet.** It turns "we handled the commands our tests happened to
need" into "every command the frontend can issue is accounted for", and makes the
next new `invoke()` fail loudly.

**T009 and T010 are reconciliation.** CI gating and the testing-authority
documents change once, at the end, when the counts and claims are real. T010
decides whether the Vite preview is rebuilt on the same state model, and it is
last because that is a convenience refactor riding on a proven harness.

## Non-goals

- Replacing component tests. The CUJ suite must not re-assert every
  component-level contract; both tiers stay.
- Adding a Playwright suite. The browser tier is scoped by T009 to what jsdom
  cannot establish, and building it is separate work.
- Changing production behavior. Bugs found by new CUJs are reported; fixing them
  is separate work unless the fix is required to make the seam function.
- Testing the Rust side. `cargo test` boundaries are unchanged.

## Promotion rule

The epic is promoted only when every implementation, contract, and documentation
packet is complete. One task cycle never promotes unrelated work.

**Status at the H01-T010 landing:** T001–T010 are complete. The second fake is
gone: the Vite preview installs the harness at startup, so the repository holds
one desktop fake-backend state machine with two front doors.

Concretely, this epic is complete when:

1. `pnpm test:desktop:cuj` boots the real `App` against the harness and covers
   the review, analysis, generation, chronology, import, and failure journeys;
2. every command reachable from `api.ts` and `chronologyApi.ts` has an explicit
   handler or an explicit "unsupported, and here is why" entry;
3. no CUJ depends on wall-clock delay, real randomness, or ambient demo data;
4. `AGENTS.md` §12, `docs/integration-testing.md`, and `.github/workflows/ci.yml`
   describe the tier that actually exists, with no claim that it proves native,
   media, or domain behavior.

## Cross-cutting verification

```bash
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
pnpm --dir apps/desktop build
pnpm check:docs
```
