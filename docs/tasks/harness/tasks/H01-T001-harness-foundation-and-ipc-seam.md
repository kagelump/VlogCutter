# H01-T001: Harness foundation and IPC seam

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "H01-T001",
  "epic_id": "H01",
  "title": "Harness foundation and IPC seam",
  "slug": "harness-foundation-and-ipc-seam",
  "status": "ready",
  "task_class": "foundation",
  "depends_on": [],
  "principal_invariant": "The real App boots in jsdom through the real api.ts and chronologyApi.ts adapters, every Tauri call it makes is answered by an explicit harness handler against one canonical state object, and no unhandled command passes silently.",
  "central_falsification": "Point a production adapter at a command name or argument shape the harness does not implement and prove the CUJ suite fails loudly rather than rendering a plausible screen.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "window.__TAURI_INTERNALS__ -> @tauri-apps/api",
    "api.ts/chronologyApi.ts -> harness command dispatch",
    "harness event transport -> React effect subscriptions"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/harness/ARCHITECTURE.md", "docs/tasks/harness/EPIC.md"],
    "implementer": ["docs/tasks/harness/ARCHITECTURE.md", "AGENTS.md", "apps/desktop/src/api.ts", "apps/desktop/src/chronologyApi.ts"],
    "independent_tester": ["docs/tasks/harness/ARCHITECTURE.md", "docs/integration-testing.md"],
    "reviewer": ["docs/tasks/harness/ARCHITECTURE.md", "AGENTS.md", "docs/integration-testing.md"],
    "integrator": ["docs/tasks/harness/EPIC.md", "AGENTS.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/harness/tasks/H01-T001-harness-foundation-and-ipc-seam.md"],
    "implementer": ["apps/desktop/src/test/harness/**", "apps/desktop/src/test/setup.ts", "apps/desktop/vitest.config.ts"],
    "independent_tester": ["apps/desktop/src/test/cuj/boot.cuj.test.tsx"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "forbidden_paths": ["core/**", "apps/desktop/src-tauri/**", "apps/web/**", "apps/server/**", "apps/cli/**", "packages/**"],
  "verification_commands": [
    "pnpm --dir apps/desktop test",
    "pnpm --dir apps/desktop check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/harness/EPIC.md",
      "regex": "\\| \\[H01-T001\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Build the foundation every later packet depends on: a fake Tauri runtime that the
production `@tauri-apps/api` reaches on its own, a single canonical state object
behind it, a logical clock, a scriptable event transport, fault injection, a
command log, and a scenario builder — plus enough command handlers to boot the
real `App` and open a project.

## Principal invariant

The real `App` renders in jsdom with `api.ts` and `chronologyApi.ts` taking their
**Tauri** branches, every command they issue during boot is answered by a named
handler reading one canonical state object, and any command without a handler
throws an error naming the command.

## Inputs and outputs

Inputs:

- `apps/desktop/src/api.ts` and `chronologyApi.ts` as the frozen call surface.
- `apps/desktop/src/types.ts` and `chronologyTypes.ts` as the DTO authority.
- `@tauri-apps/api` v2's three window globals (`invoke`, `transformCallback`,
  `convertFileSrc`) and `__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener`.

Outputs:

- `src/test/harness/` — `DesktopHarness.ts`, `ipc.ts`, `state.ts`, `events.ts`,
  `clock.ts`, `faults.ts`, `media.ts`, `builders.ts`, `commands/`, `scenarios/`.
- One boot CUJ proving the seam.
- No change to production source.

## Frozen decisions

- The fake is installed on `window.__TAURI_INTERNALS__`, not by module mocking.
  Installing it also flips `api.ts`'s `inTauri()`, so preview branches are dead
  code inside a CUJ.
- `listen`/`unlisten` are served as the commands they really are
  (`plugin:event|listen`, `plugin:event|unlisten`) plus a `transformCallback`
  registry, so listener teardown is real.
- `plugin:dialog|open` is a harness-scriptable command; native dialogs are never
  claimed to be tested.
- Handlers are registered per command name in a table. There is no fallthrough.
- All timestamps come from `harness.clock`. No `new Date()`, `Math.random()`, or
  `crypto.randomUUID()` in harness code.
- Command arguments arrive exactly as the adapter sent them; handlers must not
  normalise casing to hide an adapter bug.
- The harness never returns a success that leaves canonical state unchanged.

## Owned paths

The implementer owns `src/test/harness/**` and the two test-config files. The
independent tester owns the boot CUJ and may add fixtures under its own file
only. Production source under `apps/desktop/src/*.ts(x)` outside `test/` is not
owned by this task; a required production change is a stop condition.

## Required interface

```ts
const harness = desktopScenario()
  .project("Hokkaido field test", { id: "project_hokkaido" })
  .camera("Sony A7C II", { clips: 3, withProxy: true })
  .camera("DJI Pocket 3", { clips: 2 })
  .selectedClip("asset_sony_001")
  .build();

harness.install();          // installs window globals; idempotent per test
harness.uninstall();        // restores; called from afterEach
harness.state;              // canonical state, readable and writable
harness.clock.set(iso);     // logical time
harness.clock.advance(ms);
harness.emit(event, payload);            // through the real listener registry
harness.calls(command);                  // recorded argument objects
harness.commandLog();                    // [{ command, args, outcome }]
harness.next(command).reject(message);   // one-shot fault
harness.command(command).delayUntilReleased();
harness.release(command);
```

## Acceptance oracle

1. `harness.install(); render(<App />)` reaches a state where the project's
   clips are listed, with `api.isDesktop()` true inside the test.
2. Every command issued during boot appears in `harness.commandLog()`, and the
   log contains no entry the handler table does not name.
3. Invoking an unregistered command rejects with a message containing the
   command name and the words `test harness`.
4. `set_removed` through the UI changes `harness.state`, and a subsequent
   `list_assets` returns the changed asset.
5. A listener registered by `api.onProxyProgress` receives `harness.emit`
   payloads, and unmounting `App` unregisters it — a later emit reaches nobody.
6. Two runs of the same scenario produce identical ids, paths, and timestamps.
7. No test in the suite waits on a real timer to make progress.

## Independent test charter

Falsify the seam, not the fixtures. Try: renaming a command in a local copy of
the adapter and proving the suite fails; sending an argument in snake_case and
proving it is not silently accepted; emitting an event after unmount and proving
no state updates; asserting the harness cannot be installed twice without
`uninstall`. Build fixtures from a different scenario than the implementer's.

## Review charter

Risk domain: a fake that is more permissive than the real backend. Ask: can any
handler report success without a state change? Can any command return `null` by
default? Is any timestamp non-deterministic? Does any harness file import
production state or a component? Severity: a permissive default or a
non-deterministic identity is blocking; naming and file layout are advisory.

## Non-goals

- Handlers for analysis, chronology, edit-plan, or import beyond what boot needs.
- Any CUJ beyond the boot proof.
- Changing `api.ts`'s preview branches.
- CI wiring.

## Stop conditions

Stop and escalate when:

- `App` cannot boot without a production change.
- The installed globals are insufficient for `@tauri-apps/api` (for example a
  future version reads a different global).
- A command's argument shape cannot be answered without duplicating domain
  logic.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
```

Both must pass with the existing 374 tests unchanged.

## Handoff

List changed paths, the command handlers implemented, the commands deliberately
left unimplemented, the exact test counts before and after, and any production
defect observed while booting. The implementer does not approve the task.
