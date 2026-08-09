# H01-T008: IPC contract coverage suite

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "H01-T008",
  "epic_id": "H01",
  "title": "IPC contract coverage suite",
  "slug": "ipc-contract-coverage-suite",
  "status": "complete",
  "task_class": "qa",
  "depends_on": ["H01-T004", "H01-T005"],
  "principal_invariant": "Every Tauri command the desktop frontend can issue is either handled explicitly by the harness or listed with a stated reason, and the set of commands the frontend issues is checked against the set the Rust side registers.",
  "central_falsification": "Add an invoke of a new command name to a production adapter and prove the contract suite fails, naming the command, without any CUJ having to exercise it.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "frontend invoke surface -> harness handler table",
    "harness handler table -> Rust invoke_handler registration"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/harness/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/harness/ARCHITECTURE.md", "apps/desktop/src/api.ts", "apps/desktop/src/chronologyApi.ts"],
    "independent_tester": ["docs/tasks/harness/ARCHITECTURE.md"],
    "reviewer": ["docs/tasks/harness/ARCHITECTURE.md", "AGENTS.md"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/harness/tasks/H01-T008-ipc-contract-coverage-suite.md"],
    "implementer": ["apps/desktop/src/test/harness/contract.ts", "apps/desktop/src/test/cuj/contract.test.ts"],
    "independent_tester": ["apps/desktop/src/__independent__/ipc-contract.independent.test.ts"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "forbidden_paths": ["core/**", "apps/desktop/src-tauri/**", "apps/web/**", "apps/server/**", "apps/cli/**"],
  "verification_commands": [
    "pnpm --dir apps/desktop test",
    "pnpm --dir apps/desktop check"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/harness/EPIC.md",
      "regex": "\\| \\[H01-T008\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Turn the harness from "handles what our tests needed" into a checked contract, so
that the next new `invoke()` cannot quietly go untested.

## Principal invariant

The three sets — commands the frontend issues, commands the harness handles, and
commands the Rust side registers — are compared by a test, and every difference
is either fixed or recorded with a reason.

## Inputs and outputs

Inputs: `api.ts` and `chronologyApi.ts` as the frontend surface; the harness
handler table; `apps/desktop/src-tauri/src/lib.rs`'s `invoke_handler!` list as
the backend surface.

Outputs: a contract module that extracts each set, a suite asserting the
relationships, and an explicit exemption list with a stated reason per entry.

## Frozen decisions

- The frontend set is extracted by reading the adapter sources, not by
  maintaining a hand-written duplicate list that can drift.
- The backend set is extracted from the Rust registration block by the same
  mechanism. The comparison is a warning-free assertion, not a printed report.
- Exemptions are a literal list of `{ command, reason }`. An empty reason fails.
- The suite asserts that dispatching an unregistered command rejects with a
  message naming it — the property every other test depends on.
- Argument-shape checks assert the keys an adapter sends per command, so a
  renamed or dropped argument fails here even when no CUJ covers that flow.

## Owned paths

The implementer owns the contract module and its suite. The independent tester
owns one file under `__independent__/`.

## Required interface

```ts
frontendCommands();        // Set<string> parsed from the adapters
harnessCommands();         // Set<string> from the handler table
backendCommands();         // Set<string> parsed from the Rust registration
contractExemptions;        // [{ command, reason }]
```

## Acceptance oracle

1. Every frontend command is handled by the harness or exempt with a reason.
2. Every frontend command is registered on the Rust side — a frontend call to a
   command that does not exist would fail at runtime, and this catches it in
   jsdom.
3. A Rust command with no frontend caller is reported but not failed, since the
   CLI and tests may be its only callers.
4. Dispatching an unregistered command rejects with a message containing the
   command name.
5. For each handled command, the recorded argument keys from at least one CUJ or
   contract case match the handler's declared argument shape.
6. Adding a fabricated `invoke("nonexistent_command")` to a scratch adapter copy
   makes the suite fail — proven by the independent tester, not asserted by
   comment.

## Independent test charter

Try to defeat the extraction: an `invoke` with a templated name, one behind a
conditional, one in a helper function, one with the command name in a constant.
Report which forms the extractor cannot see, and make the suite fail closed for
those rather than silently pass.

## Review charter

Risk domain: a contract that looks like coverage but is a duplicate list.
Ask: is any set hand-maintained? Can a drift pass? Is any exemption reason
empty or generic? Severity: a hand-maintained frontend list is blocking.

## Non-goals

- Type-level DTO validation against Rust structs, which is a larger piece of
  work and would need generated schemas.
- Changing the Rust registration.

## Stop conditions

Stop and escalate when the frontend surface cannot be extracted reliably and the
only alternative is a maintained duplicate.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
```

## Handoff

Report the three sets with counts, the exemptions and their reasons, the invoke
forms the extractor cannot see, and how the suite fails closed for them.
