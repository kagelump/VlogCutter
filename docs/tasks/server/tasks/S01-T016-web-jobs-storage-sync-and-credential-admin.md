# S01-T016: Web Jobs, Storage, Sync, and Credential Administration

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T016",
  "epic_id": "S01",
  "title": "Web Jobs, Storage, Sync, and Credential Administration",
  "slug": "web-jobs-storage-sync-and-credential-admin",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "S01-T003",
    "S01-T004",
    "S01-T005",
    "S01-T006",
    "S01-T009",
    "S01-T011",
    "S01-T013"
  ],
  "principal_invariant": "The user can monitor and manage server jobs, storage, synchronization state, credentials, and scheduled scans through the Chrome web UI — every administrative view uses the typed client and no action has an unconfirmed destructive effect.",
  "central_falsification": "View the job queue in Chrome, verify job states, progress, and ETA are displayed. Cancel a job and verify the cancellation is durable (survives refresh). View storage root capacity and binding state. View sync status per project. Revoke a device credential and verify it takes effect. Create a scheduled scan and verify it persists.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Web admin → job engine (S01-T004)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T016-web-jobs-storage-sync-and-credential-admin.md"
    ],
    "implementer": [
      "apps/web/src/**",
      "packages/project-client/**"
    ],
    "independent_tester": [
      "apps/web/e2e/**"
    ],
    "integrator": []
  },
  "forbidden_paths": [
    "apps/desktop/**",
    "core/**",
    "apps/server/**",
    "deploy/**"
  ],
  "verification_commands": [
    "pnpm --dir apps/web build",
    "pnpm --dir apps/web typecheck",
    "pnpm test:web",
    "pnpm test:web:e2e",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 160 \\| \\[S01-T016\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement web-based administration for jobs, storage, synchronization,
credentials, and scheduled scans — every administrative surface the server needs
for day-to-day operation.

## Principal invariant

This task is correct when the user can monitor and manage server jobs, storage,
synchronization state, credentials, and scheduled scans through the Chrome web
UI — every administrative view uses the typed client and no action has an
unconfirmed destructive effect.

## Inputs and outputs

Inputs:
- S01-T003 storage roots and global media library
- S01-T004 durable job engine with progress, ETA, and event notifications
- S01-T005 scan infrastructure
- S01-T006 representation store
- S01-T009 authentication, credentials, and sessions
- S01-T011 sync status and diagnostics
- S01-T013 web foundation and typed client

Outputs:
- Job queue view with state, progress, ETA, and cancellation
- Storage administration: roots, bindings, capacity, availability
- Sync administration: per-project status, cursors, and reconciliation state
- Credential administration: list, create, revoke devices and browser sessions
- Scheduled scan management: create, list, pause, cancel schedules
- All destructive actions confirmed and reversible where applicable

## Frozen decisions

1. Job cancellation must be confirmed and is durable (survives refresh).
2. Credential revocation must be confirmed; once revoked, a credential cannot be un-revoked.
3. Storage root disabling preserves history and project links (from S01-T003).
4. Administrative views are read-only by default; mutations require explicit confirmation.
5. All administration uses the typed client from S01-T013.
6. Credentials never appear in full in the UI (display partial identifiers only).

## Owned paths

- **Implementer:** `apps/web/src/` (admin views: jobs, storage, sync, credentials, schedules), `packages/project-client/` (admin DTOs)
- **Independent tester:** `apps/web/` (e2e: each admin view)
- **Integrator:** no new integration paths

## Required interface

- Job queue and cancellation API
- Storage root and binding management API
- Sync status API
- Credential management API
- Scan schedule API

## Acceptance oracle

1. Job queue displays states, progress, and ETA for active jobs.
2. Cancelling a job is durable and survives page refresh.
3. Storage view shows roots, bindings, capacity, and availability.
4. Sync view shows per-project status, cursors, and reconciliation state.
5. Credential view lists all devices and sessions with partial identifiers.
6. Revoking a credential takes effect and is irreversible.
7. Scheduled scan can be created, viewed, paused, and cancelled.
8. All destructive actions require confirmation.
9. No credential appears in full in the UI.

## Independent test charter

Try to falsify: "destructive actions are confirmed and durable." Attempt to
cancel a job without confirmation (verify confirmation is required). Cancel a
job with confirmation, refresh, verify the cancellation survived. Revoke a
credential, attempt to use it, verify rejection. Attempt to disable a storage
root without confirmation. Verify that partial credential identifiers cannot be
used to reconstruct the full credential.

## Review charter

Risk domain: administration safety. Questions: (1) Can a destructive action be
performed without confirmation? (2) Does credential revocation take effect? (3)
Are full credentials ever displayed? (4) Does job cancellation survive refresh?
Severity: P0 for unconfirmed destructive actions, credential exposure, or
revocation failure; P1 for non-durable cancellation or incomplete sync status.
Approval vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Server configuration editing through web UI
- Database migration through web UI
- Multi-server federation management
- Usage analytics or billing

## Stop conditions

Stop and escalate when:

- A destructive action can be performed without explicit confirmation
- Full credentials are displayed in the UI
- Credential revocation is not immediate
- Job cancellation is not durable across refresh

## Commands

```sh
pnpm --dir apps/web build
pnpm --dir apps/web typecheck
pnpm test:web
pnpm test:web:e2e
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Web administration completes the Chrome product surface. Consumed by S01-T019
(end-to-end acceptance) and S01-T020 (documentation).
