# Desktop CUJ test harness — architecture

Status: accepted design, implemented (epic `H01`, tasks T001–T009).
Scope: `apps/desktop`. Nothing here changes `core/`, `apps/server`, or
`apps/cli`.

## 1. The gap this closes

The desktop already has strong coverage at both ends and almost nothing in the
middle.

At the bottom: `apps/desktop/src/review/*.test.ts` covers ordering, keymaps,
formatting, geometry, rotation, and presentation state as pure functions, and
`apps/desktop/src/components/__tests__/*` renders individual components against
hand-built assets and callback spies. At the far end: `cargo test --workspace`
and `pnpm test:integration` prove the domain — catalog, ingest, chronology,
persistence, media processing — against real SQLite, real processes, and real
FFmpeg.

Between them there was a single suite that rendered the whole `App`
(`shell.test.tsx`, `focus.test.tsx`, `judgmentRail.test.tsx`), and it ran
against whatever demo data happened to live inside `api.ts`'s `if (!inTauri())`
branches. Those branches existed for the Vite preview at port 1420. They were
never designed as a test contract, and they behaved in ways a test contract must
not:

- `generateMissingProxies` reports `{ generated: 2, skipped: 5 }` and changes no
  asset. A later `listAssets` still shows every clip without a proxy. A test
  that asserted "two clips became playable" would pass against a lie.
- `mediaUrl` returns `null` outside Tauri, so no full-App test can reach any
  media behavior at all.
- `chronologyApi` deliberately returns empty overviews and `null` writes outside
  Tauri — the right call for a preview, since a second chronology solver in
  TypeScript could disagree with `rcdvlog-core` — but it means the entire
  calibration and edit-plan surface is untestable below the native tier.
- `mockAnalyzeProject` sleeps on real timers, 140 ms per clip per stage.
- `ingestFolder`, `generateProxy`, and `relinkFolder` call `invoke` with no
  browser branch at all, so they throw in jsdom.

The result was that the questions users actually ask could not be tested
cheaply:

> Open a project, review several clips, add a note, label one, remove another,
> switch filters, come back — is everything still coherent?

**Those branches are gone.** H01-T010 deleted them: the preview installs this
harness at startup (§11), the three whole-`App` component suites boot against a
declared scenario, and every list above is now a description of history rather
than of `api.ts`.

## 2. Design principle

> **Fake the environment, not the application.**

The fast suite runs as much production frontend code as it can and replaces only
the boundary that is expensive or nondeterministic.

| Real in a CUJ test | Faked | Proven elsewhere |
| --- | --- | --- |
| React components, `App` orchestration | Tauri IPC destination | Domain results (Rust) |
| `api.ts`, `chronologyApi.ts` | filesystem, media processing | Native media (Tauri tier) |
| Command names and argument shapes | network and server | Real layout (browser tier) |
| Event subscription and teardown | background timing | FFmpeg (integration tier) |
| Frontend state transitions | native dialogs | |

## 3. The seam

The fake goes **below** `api.ts`, not in place of it.

```text
production                              fast CUJ test

App.tsx                                 App.tsx
  ↓                                       ↓
api.ts / chronologyApi.ts               api.ts / chronologyApi.ts     ← real
  ↓                                       ↓
@tauri-apps/api invoke()/listen()       @tauri-apps/api invoke()/listen()  ← real
  ↓                                       ↓
window.__TAURI_INTERNALS__              window.__TAURI_INTERNALS__    ← the fake
  ↓                                       ↓
Rust commands                           DesktopHarnessState
  ↓
rcdvlog-core
```

This works because `@tauri-apps/api` is a thin shim over three window globals,
and every one of them is installable from a test:

```js
// node_modules/@tauri-apps/api/core.js
async function invoke(cmd, args = {}, options) {
  return window.__TAURI_INTERNALS__.invoke(cmd, args, options);
}
function convertFileSrc(filePath, protocol = 'asset') {
  return window.__TAURI_INTERNALS__.convertFileSrc(filePath, protocol);
}
function transformCallback(callback, once = false) {
  return window.__TAURI_INTERNALS__.transformCallback(callback, once);
}
```

`listen(event, handler)` is itself a command: it registers the handler through
`transformCallback` and then invokes `plugin:event|listen` with the resulting
id. `unlisten` calls `window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener`
and invokes `plugin:event|unlisten`. `@tauri-apps/plugin-dialog`'s `open()` is
`invoke('plugin:dialog|open', { options })`. So installing one object gives the
harness commands, events, dialogs, and asset URLs through the same door.

Installing `__TAURI_INTERNALS__` also flips `api.ts`'s own `inTauri()` check.
That is what let H01-T010 delete the browser-preview branches outright instead
of replacing them: the preview installs the harness through this same seam, so
the adapters have one path in every environment (§11).

### 3.1 Why not mock `api.ts`

`vi.mock("../api")` would be less code and would test less. Faking below the
adapters keeps these under test on every CUJ:

- command names (`create_note_revision`, not `createNoteRevision`);
- argument names and casing (`{ assetId, patch }`, camelCase across the bridge);
- missing or extra arguments;
- adapter regressions such as `addLabel` sending a whole patch;
- event names (`proxy-progress`, `analysis-progress`, `sync-status-changed`);
- listener teardown on unmount;
- the read/write sequencing `App` performs around a mutation.

Those are exactly the defects that ship green when the adapter is the mock.

### 3.2 Why not Playwright for this

Nothing in the list above needs a browser engine. Chromium proves layout,
scroll, real focus, and real media; it costs seconds per scenario and a browser
download in CI. The fast tier's job is to make 50+ journeys cheap enough that
nobody weighs whether a regression test is worth it.

## 4. Harness structure

```text
apps/desktop/src/test/
  harness/
    DesktopHarness.ts     install/uninstall, invoke dispatch, command log
    state.ts              DesktopHarnessState + coherent mutators
    ipc.ts                the __TAURI_INTERNALS__ implementation
    events.ts             transformCallback registry, emit, listener teardown
    clock.ts              logical clock; every fake timestamp comes from here
    faults.ts             per-command rejection, delay, release
    media.ts              synthetic media-element events by asset id
    builders.ts           deterministic Asset/Project/transcript builders
    chronologyFixtures.ts declared chronology outcomes, never computed ones
    scenario.ts           the scenario builder
    renderApp.tsx         boot the real App and read the queue back
    contract.ts           the three command surfaces, extracted not listed
    commands/             one module per command family, all explicit
  cuj/
    *.cuj.test.tsx        full-App journeys
    contract.test.ts      the ratchet: no unhandled or unregistered command
```

### 4.1 One canonical state

Every fake command reads and writes a single object.

```ts
interface DesktopHarnessState {
  settings: DesktopSettings;
  projects: Project[];
  assets: Record<string, Asset>;
  transcripts: Record<string, ClipTranscript>;
  analyses: Record<string, AssetAnalysisView>;
  analysisRuns: AnalysisRunRecord[];
  models: ModelStatus[];
  openRouterKeyPresent: boolean;
  chronology: ScenarioChronologyState;
  diagnostics: Diagnostic[];
  editPlans: ScenarioEditPlanState;
  archiveRoots: ArchiveRoot[];
  cards: CardCandidate[];
  pairing: ScenarioPairingState;
  sync: Record<string, SyncStatus>;
  jobs: ScenarioJobState;
}
```

The load-bearing rule:

> **A successful mutating command must change canonical state so that every
> later read observes it.**

`set_removed(asset_3, true)` sets `state.assets.asset_3.removed`, and the next
`list_assets` returns it. `generate_missing_proxies` marks each asset
`proxyAvailable` and gives it a `proxyPath`, and the returned count equals the
number of assets it changed. A handler that reports work it did not do is a bug
in the harness, and §7 makes that assertable.

### 4.2 Explicit commands only

There is no permissive default. An unhandled command throws:

```text
Unhandled Tauri command in test harness: analyze_project_paths
  Add a handler in src/test/harness/commands/, or assert the rejection.
```

Adding a production `invoke()` therefore fails the CUJ suite until the harness
learns the command. That is the mechanism that keeps the fake honest as the app
grows; a `return null` default would let the fake rot silently.

### 4.3 Deterministic identity

Fixture ids, paths, times, and costs are stable across runs. No
`crypto.randomUUID()`, no `new Date()`, no `Math.random()` in harness code. Ids
read like `project_spain`, `asset_sony_001`; times come from the harness clock,
which starts at a fixed instant per scenario.

## 5. Determinism

### 5.1 Clock

The harness owns a logical clock. Handlers stamp `createdAt`, `pairedAt`,
`lastContextUpdatedAt`, and run timestamps from it, and tests move it:

```ts
harness.clock.set("2026-08-09T12:00:00Z");
harness.clock.advance(90_000);
```

The logical clock is deliberately separate from Vitest's fake timers.  `App`
uses `window.setTimeout`/`setInterval` in three places (a save-debounce, a
15-second sync poll, a 4-second message expiry). A test that cares about those
opts into `vi.useFakeTimers()` and drives `userEvent` with `advanceTimers`; a
test that does not gets a stable clock without paying for timer interception.

### 5.2 Jobs are scripted, never slept

Long operations do not resolve on their own. The test decides when each
progress event lands and how the operation ends:

```ts
const run = harness.job("analyze_project");
await user.click(screen.getByRole("button", { name: "Analyze" }));

await run.progress({ stage: "proxies", completed: 3, total: 20 });
await run.progress({ stage: "analysis", completed: 20, total: 20 });
await run.complete({ analyzed: 20, costUsd: 0.0032 });
```

`await run.progress(...)` emits the real Tauri event through the real listener
registered by `api.onAnalysisProgress`, wrapped in `act`. The command promise
stays pending until `complete`/`fail`/`cancel`, which is what makes cancellation
races and "still running" UI states testable at all.

### 5.3 Faults are first class

```ts
harness.next("update_asset").reject("database is locked");
harness.command("sync_now").delayUntilReleased();
harness.asset("asset_sony_003").setOriginalAvailable(false);

// Argument-matched, so one project's read can be held while another's is
// answered — the session race in #28, staged exactly rather than with timers.
harness.command("list_assets", (args) => args.projectId === "project_a").delayUntilReleased();
harness.command("list_assets", (args) => args.projectId === "project_a")
  .delayThenReject("catalog unavailable");
harness.release("list_assets"); // frees every call held for that command
```

Failure paths are the strongest argument for this harness: a note-save failure
that loses a draft, an offline volume mid-review, a stale edit-plan revision,
and a cancelled analysis that reports completion are all cheap here and
expensive or impossible to stage against real hardware.

## 6. What the harness refuses to fake

### 6.1 No second chronology solver

`chronologyApi` scenarios describe **backend outcomes**, not rules. A scenario
states the overview the solver returns, and optionally what a named edit makes
it return next:

```ts
scenario
  .chronology({ order: ["sony-001", "dji-001", "sony-002"] })
  .whenClockEpochApplied("sony-seq-1", { offsetSeconds: 3600 })
  .thenChronology({ order: ["dji-001", "sony-001", "sony-002"] });
```

The CUJ then proves the UI loads chronology, previews before writing, shows the
impact, writes only on confirmation, refreshes, and restores queue position.
Whether a 3600-second epoch correction actually produces that order is proven by
`cargo test -p rcdvlog-core`, and by nothing in TypeScript. The same applies to
diagnostics, edit-plan generation, validation, and OTIO export.

### 6.2 No video decoding

jsdom has no media pipeline. Media in the fast tier is **logical playback
state**: the harness dispatches real DOM events at the real mounted `<video>`
element.

```ts
harness.media("asset_sony_001").loaded({ duration: 12.4 });
harness.media("asset_sony_001").playing();
harness.media("asset_sony_001").failed("decode");
```

That covers play/pause orchestration, buffering UI, position bookkeeping,
neighbor preloading, keyboard transport, and error recovery calls. It says
nothing about whether WKWebView paints a first frame — which is precisely why
`docs/tasks/desktop/MEDIA_DELIVERY.md`'s three acceptance criteria stay in the
native tier.

`mediaUrl` does become exercisable: the harness returns a `mediaOrigin` in
`desktop_settings`, so the production URL-construction path (representation
choice, token, id encoding) runs for real and is assertable.

## 7. Observability

The harness records every command with its arguments and its outcome.

```ts
expect(harness.calls("preview_clock_epoch")).toHaveLength(1);
expect(harness.commandLog().map((entry) => entry.command)).toEqual([
  "capture_queue_position",
  "preview_clock_epoch",
  "set_clock_epoch",
  "chronology_overview",
  "restore_queue_position",
]);
```

This is for ordering contracts — *preview before write*, *capture before
mutate* — and for debugging. Ordinary UI behavior is asserted through what the
user sees; a CUJ whose only assertion is a command log is testing the harness.

## 8. Test tiers and their responsibility boundary

```text
pure/unit           ~250          review/*.test.ts
component           ~120          components/__tests__/*
fast full-App CUJ   131           cuj/*.cuj.test.tsx      ← this epic
real browser        18            e2e/*.spec.ts           ← built (#22)
native Tauri        < 20          real IPC, Keychain, dialogs, WKWebView frames
real media/manual   small         FFmpeg, NAS, sleep/wake, Resolve
```

Every test above the fast tier must answer: *what real-browser or real-native
property does this prove that a CUJ cannot?* If there is no answer, it moves
down.

The fast tier is not a replacement for component tests. A component test asks
"does clicking this row call `onSelect("b")`?"; a CUJ asks "when the user
selects another clip, do the player, annotation state, evidence shelf, selection
persistence, and subsequent commands all agree?" Both are worth keeping, and the
CUJ suite should not re-assert every component-level contract.

## 9. What a CUJ may not claim

A green fast suite is never evidence for: WKWebView behavior, video decoding,
loopback transport, filesystem permissions, native dialogs, Keychain, sleep/wake,
NAS mount/disconnect, FFmpeg correctness, the chronology solver, edit-plan
generation, or Rust persistence and migrations. `docs/integration-testing.md`
remains the authority on which boundary proves what, and this epic adds a row
rather than reassigning existing ones.

## 10. CI

```text
pnpm test:desktop        unit + component + CUJ (every PR)
pnpm test:desktop:cuj    the CUJ tier alone (iteration, and its own CI step)
pnpm test:desktop:browser  the real-browser tier in headless Chromium (#22)
cargo test -p rcdvlog-desktop   the Tauri command layer (macOS job)
```

As built: 131 CUJ scenarios across 14 files, ~24s on their own and ~38s inside
the full desktop suite of 505 tests.

The CUJ tier is jsdom-only: no Chromium download, no Tauri toolchain, no
FFmpeg, no network. It runs unchanged on the existing ubuntu CI job, in a
container, or on a laptop.

## 11. Relationship to the browser preview

There is one fake backend, reached through two front doors.

`main.tsx` checks for `__TAURI_INTERNALS__` and, when it is absent, loads
`src/preview/` through a dynamic `import()` and installs a `DesktopHarness`
built from a declared scenario. From that moment `inTauri()` is true, so the
preview runs the real adapters against the real command table — the same one the
CUJ suite drives. A behaviour change in `src/test/harness/commands/` is
observable from both doors, and neither `api.ts` nor `chronologyApi.ts` has an
environment branch left to disagree with itself.

Three rules keep the preview out of the test tier:

- **The harness never imports the preview.** The dependency runs one way, and
  `src/__independent__/preview-parity.independent.test.ts` asserts it.
- **`act` is injected, not imported.** `DesktopHarness` takes an `ActWrapper`
  that defaults to a passthrough, and `renderApp` supplies the
  `@testing-library/react` version. That keeps a devDependency out of the
  shipped bundle, and it is why the dynamic `import()` matters: the preview
  backend is its own chunk, which the desktop build never fetches.
- **Preview-only non-determinism lives in `src/preview/liveJobs.ts`.** It claims
  the same scriptable jobs a CUJ claims and releases their steps on real timers,
  which is what makes progress and cancel worth looking at. Nothing a CUJ loads
  contains a `setTimeout`.

What the preview still is not: evidence. It proves nothing about SQLite,
WKWebView, Keychain, native dialogs, or FFmpeg. Its settings deliberately carry
no media origin, because nothing is serving those bytes — so every clip reports
no URL, which is the state the desktop is in when the origin fails to bind.
