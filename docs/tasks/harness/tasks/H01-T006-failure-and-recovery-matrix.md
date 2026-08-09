# H01-T006: Failure and recovery matrix

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "H01-T006",
  "epic_id": "H01",
  "title": "Failure and recovery matrix",
  "slug": "failure-and-recovery-matrix",
  "status": "ready",
  "task_class": "qa",
  "depends_on": ["H01-T002", "H01-T003"],
  "principal_invariant": "Every backend failure the desktop can encounter leaves the user's unsaved work intact, their place in the queue intact, and an accurate, actionable statement of what failed — and a retry after the fault clears succeeds.",
  "central_falsification": "Fail a save while a draft is on screen and prove the draft is still there, still editable, and still saveable, rather than silently replaced by the last persisted value.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "fault injection -> adapter rejection",
    "rejected command -> error surface and retained draft",
    "recovery -> successful retry through the same command"
  ],
  "documents_by_role": {
    "contract_editor": ["docs/tasks/harness/ARCHITECTURE.md"],
    "implementer": ["docs/tasks/harness/ARCHITECTURE.md", "docs/desktop/HARD_LESSONS.md", "AGENTS.md"],
    "independent_tester": ["docs/desktop/HARD_LESSONS.md", "docs/desktop/smoke-test.md"],
    "reviewer": ["docs/tasks/harness/ARCHITECTURE.md", "AGENTS.md"],
    "integrator": ["docs/tasks/harness/EPIC.md"]
  },
  "path_ownership": {
    "contract_editor": ["docs/tasks/harness/tasks/H01-T006-failure-and-recovery-matrix.md"],
    "implementer": ["apps/desktop/src/test/harness/faults.ts", "apps/desktop/src/test/cuj/recovery.cuj.test.tsx", "apps/desktop/src/test/cuj/sync.cuj.test.tsx"],
    "independent_tester": ["apps/desktop/src/__independent__/failure-matrix.independent.test.tsx"],
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
      "regex": "\\| \\[H01-T006\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Systematically test the conditions that are expensive or impossible to stage
against real hardware, and that are therefore almost never tested: failed saves,
offline volumes, interrupted generation, stale revisions, sync failure, pairing
failure, and a backend error during project load.

## Principal invariant

For each injected failure, the CUJ asserts three things: the user's in-progress
work survives, the interface says something true and actionable about what
failed, and the same action succeeds once the fault clears.

## Inputs and outputs

Inputs: harness fault injection from H01-T001; the review, generation, sync, and
pairing command families.

Outputs: a matrix of failure CUJs, one per row of the table below, plus whatever
fault-injection API gaps they expose.

| Failure | Injected as | Recovery asserted |
| --- | --- | --- |
| Note save fails | `next("create_note_revision").reject(...)` | retry saves the same draft |
| Label write fails | `next("add_label").reject(...)` | label appears after retry |
| Project load fails | `next("list_assets").reject(...)` | reopening succeeds |
| Proxy generation fails mid-batch | job `fail(...)` after partial progress | finished prefix retained |
| Source volume offline | `asset(...).setOriginalAvailable(false)` | relink/refresh restores |
| Media element error | `media(...).failed("decode")` | recovery call, selection kept |
| Sync failure | `next("sync_now").reject(...)` | status is actionable, retry works |
| Pairing succeeds, connection fails | scripted pair then `server_connection_status` fault | user can unpair and retry |
| Stale edit-plan revision | scenario conflict outcome | no write, conflict shown |
| Slow command | `command(...).delayUntilReleased()` | busy state, no double submit |

## Frozen decisions

- Faults are per-command and one-shot by default, so a retry exercises the
  success path through the same production call.
- Every failure CUJ asserts the retained draft or retained selection explicitly;
  "no crash" is not an acceptance.
- Error text assertions check that the message names the failing operation. They
  do not lock exact copy.
- Delayed commands prove the busy state and double-submit guard, and are released
  by the test rather than by a timer.

## Owned paths

The implementer owns the fault module and the two CUJ files. The independent
tester owns one file under `__independent__/`.

## Required interface

```ts
harness.next("create_note_revision").reject("database is locked");
harness.command("sync_now").delayUntilReleased();
harness.release("sync_now");
harness.always("list_assets").reject("catalog unavailable");
harness.clearFaults();
```

## Acceptance oracle

Each row of the failure table has a named test that asserts, in order: the fault
is visible to the user, the in-progress work is intact, and the retry after
`clearFaults()` succeeds and reaches canonical state.

## Independent test charter

Combine faults rather than testing them singly: fail a save while a generation
job is running; take a volume offline during a pending sync; reject the reload
that follows a successful mutation. The invariant is the same in combination.

## Review charter

Risk domain: tests that assert an error appeared and nothing else. Ask: does
every failure test assert retained work? Does every one assert a successful
retry? Does any assert exact copy that will churn? Severity: a failure test
without a recovery assertion is blocking.

## Non-goals

- Real network, real disk, or real NAS failure modes.
- Rust-side error mapping.

## Stop conditions

Stop and escalate when a failure the table names cannot be injected at the IPC
boundary, or when a recovery path does not exist in production and would need
new behavior.

## Commands

```sh
pnpm --dir apps/desktop test
pnpm --dir apps/desktop check
```

## Handoff

Report the matrix as implemented, any row that could not be tested and why, and
every production defect the matrix exposed — these are expected and are reported,
not silently fixed.
