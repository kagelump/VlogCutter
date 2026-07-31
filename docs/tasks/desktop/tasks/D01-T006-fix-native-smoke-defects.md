# D01-T006: Fix native smoke defects

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "D01-T006",
  "epic_id": "D01",
  "title": "Fix native smoke defects",
  "slug": "fix-native-smoke-defects",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": ["D01-T005"],
  "principal_invariant": "Every defect in the native smoke ledger is resolved in the built macOS application, and every checklist row that previously passed still passes.",
  "central_falsification": "Re-run the full smoke checklist against a newly built bundle from a clean projects root with different media; every prior defect must be gone and no prior pass may regress.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/desktop/ARCHITECTURE.md", "docs/tasks/desktop/SMOKE_LEDGER.md"],
    "implementer": ["docs/tasks/desktop/SMOKE_LEDGER.md", "docs/desktop/smoke-test.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "independent_tester": ["docs/tasks/desktop/SMOKE_LEDGER.md", "docs/desktop/smoke-test.md", "AGENTS.md"],
    "reviewer": ["docs/tasks/desktop/SMOKE_LEDGER.md", "AGENTS.md", "docs/tasks/desktop/ARCHITECTURE.md"],
    "integrator": ["docs/tasks/desktop/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/desktop/tasks/D01-T006-fix-native-smoke-defects.md"],
    "implementer": ["docs/tasks/desktop/SMOKE_LEDGER.md"],
    "independent_tester": ["docs/task-cycles/D01-T006/verification/**"],
    "integrator": ["docs/task-cycles/D01-T006/evidence/**"]
  },
  "forbidden_paths": [".github/**", "core/rcdvlog-server/**", "apps/server/**", "apps/web/**", "PRD.md"],
  "verification_commands": [
    "cargo test --workspace",
    "pnpm check:frontend",
    "pnpm test:desktop",
    "pnpm --dir apps/desktop tauri build"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/desktop/EPIC.md",
      "regex": "\\| \\[D01-T006\\].*\\| complete \\|"
    },
    {
      "path": "docs/tasks/desktop/SMOKE_LEDGER.md",
      "regex": "(?s)## Resolution\\b.*defects resolved"
    }
  ]
}
-->

## Goal

Resolve every defect the native smoke acceptance found, so that Priority 1 builds
on a desktop application whose currently claimed behavior actually works.

## Principal invariant

This task is correct when every `SD-n` defect in
`docs/tasks/desktop/SMOKE_LEDGER.md` is resolved in the built macOS application,
and every checklist row previously recorded `pass` still passes.

## Inputs and outputs

Inputs:

- `docs/tasks/desktop/SMOKE_LEDGER.md` `SD-n` sections. The ledger is the
  specification. Nothing outside it is in scope.
- `docs/desktop/smoke-test.md` as the regression checklist.

Outputs:

- The minimum production change resolving each `SD-n`.
- A regression test for each `SD-n` at the lowest layer that can actually catch it
  — a `rcdvlog-core` test where the defect is in domain logic, a
  `rcdvlog-desktop` test where it is in a Tauri command, a `vitest` case where it
  is in extracted frontend logic.
- A `## Resolution` section appended to the ledger, per `SD-n`: root cause, files
  changed, the regression test added, and the re-run result, ending with the line
  `All N defects resolved`.

## Frozen decisions

- The ledger is exhaustive and closed. A defect not in it is not in this task.
- Every `SD-n` gets an automated regression test wherever the defect is reachable
  from one. A defect only reproducible through native playback, a macOS
  permission prompt, or volume ejection is exempt, and the resolution must say so
  explicitly and name why. "Hard to test" is not a reason; "not reachable without
  the native media stack" is.
- Fix the cause, not the symptom. No suppressed lint, no deleted assertion, no
  newly ignored test.
- `AGENTS.md` §15 applies: additive edits only to `core/rcdvlog-core/src/lib.rs`,
  `core/rcdvlog-core/src/operations.rs`, and `apps/desktop/src/App.tsx`.
- `apps/desktop/src/api.ts` stays the only module that calls `invoke`, per
  `AGENTS.md` §10.
- No new dependency, no new migration, and no new operation type. Any of those is
  a stop condition, because they are architecture decisions and this is a repair
  packet.

## Owned paths

`draft` for the same reason as D01-T002: the implementer path set is whatever the
`SD-n` rows name, and those do not exist yet. A packet that cannot enumerate its
paths fails admission criterion 5.

The only paths ownable today are the ones independent of the ledger's contents:
the implementer owns the ledger's `## Resolution` section, and the independent
tester and integrator own their cycle directories. The source and regression-test
paths are added at promotion.

Promotion procedure for the contract editor, once D01-T005 is complete:

1. Read every `SD-n` and list the exact files each requires changing.
2. Add that list to `path_ownership.implementer` and the regression test files
   to `path_ownership.independent_tester`.
3. If the union crosses more than one load-bearing seam — for example one defect
   in the core domain and another in the Tauri command layer — split into
   `D01-T006a`, `D01-T006b`, … before promoting. Do not promote a multi-seam
   repair packet.
4. Re-run the admission gate, then set this packet and the epic row to `ready`.

## Required interface

No new public interface, no new operation type, no new migration, no signature
change to an existing Tauri command. A fix that requires one of those is escalated
rather than absorbed.

## Acceptance oracle

1. Every `SD-n` reproduction in the ledger, replayed against a newly built bundle,
   no longer reproduces.
2. Every checklist row previously recorded `pass` still passes.
3. Every `SD-n` has a regression test that fails on the parent commit and passes
   on the fix commit, or a recorded exemption naming why it is unreachable from
   an automated test.
4. `cargo test --workspace`, `pnpm check:frontend`, `pnpm test:desktop` and
   `pnpm --dir apps/desktop tauri build` all succeed.
5. No source media file is created, modified, or deleted during verification.
6. `git diff` adds no `#[ignore]`, no `#[allow(`, no `eslint-disable`, and no
   `.skip(`.
7. `git diff --stat` touches only files traceable to a specific `SD-n`.

## Independent test charter

Falsify the claim that the native defects are gone.

- Build your own bundle from the fix commit. Do not accept the implementer's
  build.
- Use a **different** media folder and a **fresh** projects root, with different
  cameras and a different non-ASCII filename, so a fix that accidentally depends
  on the implementer's specific fixture is exposed.
- Run the entire `docs/desktop/smoke-test.md` checklist, not only the defect rows.
  A repair that breaks a neighbouring row is the realistic risk here.
- For each `SD-n` regression test, check out the parent commit, apply only the
  test, and confirm it fails. A regression test that passes without the fix tests
  nothing.
- Exercise the awkward native rows again even when no defect touched them:
  asset-protocol playback with seek, volume ejection during an active batch,
  relaunch integrity.
- Confirm originals were untouched by comparing the source file list, sizes, and
  modification times before and after.
- Grep the diff for suppression patterns and for any new dependency in
  `Cargo.toml` or `package.json`.

## Review charter

Risk domain: a repair that trades one native defect for another, or that claims a
fix without a test that would catch its return.

Questions:

1. Does each change address the root cause the ledger identified?
2. Does each `SD-n` have a regression test that genuinely fails without the fix,
   or a justified exemption?
3. Did any previously passing checklist row regress?
4. Was any assertion weakened, test removed, or lint suppressed?
5. Is any change outside the ledger's scope?
6. Did the fix introduce a dependency, migration, or operation type that should
   have been a stop condition?

Severity rules: an unresolved `SD-n`, a regressed row, a fix without a regression
test and without a stated exemption, or a suppressed check is blocking. Overly
broad but harmless edits are non-blocking with a note.

Smallest acceptable remedy: add the missing regression test, or revert the
offending hunk and fix the cause.

Approval vocabulary: `approved`, `approved with non-blocking notes`, or
`rejected: <blocking reason>`.

## Non-goals

- Implementing anything the smoke ledger recorded `not_implemented`. Those are
  D01-T007 onward.
- Improving the checklist or adding rows to it.
- Refactoring code that merely looks poor near a defect.
- Performance or scale work.
- CI changes.

## Stop conditions

Stop and escalate when:

- resolving an `SD-n` requires a new migration, a new operation type, a new
  dependency, or a change to a public behavior invariant;
- two `SD-n` rows share a root cause in a shared seam, making this task cross more
  than one seam;
- a defect turns out to be a missing feature rather than a bug, which means it
  belongs to a later packet and the ledger row was misclassified;
- a defect is in the Tauri or WebKit platform rather than this code, so the fix is
  a workaround needing a design decision;
- the only available fix weakens a check or hides an error from the user;
- a fix requires touching `core/rcdvlog-server/**`, `apps/server/**`, or
  `apps/web/**`, all forbidden here.

## Commands

```sh
cargo test --workspace
pnpm check:frontend
pnpm test:desktop
pnpm --dir apps/desktop tauri build
```

Then, per `SD-n`, to prove the regression test is load-bearing:

```sh
git stash --keep-index
cargo test --workspace 2>&1 | tail -20
git stash pop
```

Expected result: all four gate commands exit zero, and each regression test fails
when the corresponding fix is absent.

## Handoff

Report each `SD-n` with its root cause, the files changed, the regression test
added or the exemption claimed with its justification, and the full checklist
re-run results. State explicitly whether any suppression pattern or new
dependency was added. Confirm source media was untouched. List any stop condition
encountered. The implementer does not approve this task.
