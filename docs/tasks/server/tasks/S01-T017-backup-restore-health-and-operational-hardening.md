# S01-T017: Backup, Restore, Health, and Operational Hardening

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T017",
  "epic_id": "S01",
  "title": "Backup, Restore, Health, and Operational Hardening",
  "slug": "backup-restore-health-and-operational-hardening",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "S01-T002",
    "S01-T004",
    "S01-T008",
    "S01-T009"
  ],
  "principal_invariant": "A verified database backup can be restored to a clean server, and after restore the server reconciles sync state without data loss — plus structured health/readiness, log levels, and graceful degradation under resource pressure.",
  "central_falsification": "Take a verified backup of a running server with project data, wipe the control database, restore from backup, restart the server, and verify the server identity, project registries, and sync cursors are intact. Then trigger a sync and verify the restored server reconciles correctly with the desktop.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Backup → control catalog (S01-T002)"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/OPERATIONS.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/OPERATIONS.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/OPERATIONS.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T017-backup-restore-health-and-operational-hardening.md"
    ],
    "implementer": [
      "core/rcdvlog-server/src/**",
      "apps/server/src/**"
    ],
    "independent_tester": [
      "core/rcdvlog-server/tests/**",
      "apps/server/tests/**"
    ],
    "integrator": [
      "deploy/server/compose.yaml"
    ]
  },
  "forbidden_paths": [
    "apps/desktop/**",
    "apps/web/src/**",
    "apps/mobile/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server",
    "cargo test -p rcdvlog-server --test integration",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 170 \\| \\[S01-T017\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Implement verified database backup and restore, structured health/readiness
reporting, and operational hardening: log levels, resource pressure handling,
and recovery from server disk loss.

## Principal invariant

This task is correct when a verified database backup can be restored to a clean
server, and after restore the server reconciles sync state without data loss —
plus structured health/readiness, log levels, and graceful degradation under
resource pressure.

## Inputs and outputs

Inputs:
- S01-T002 server control catalog
- S01-T004 durable job engine (backup scheduling)
- S01-T008 sync protocol (restore reconciliation)
- S01-T009 authentication (backup encryption)

Outputs:
- Scheduled database backup with verification (SQLite `.dump` or VACUUM INTO)
- Backup inspection and integrity check
- Restore procedure with post-restore sync reconciliation
- Structured health/readiness endpoints with component-level detail
- Configurable log levels with runtime adjustment
- Graceful degradation under disk pressure
- Reseed/rescan recovery path for lost derived media

## Frozen decisions

1. Backups are verified on creation; unverified backups are not trusted.
2. Restore does not automatically resume sync — it requires explicit confirmation.
3. Post-restore sync reconciliation is bounded and reports discrepancies.
4. Health/readiness is read-only and never mutates state.
5. Backup schedule is configurable with retention policy.
6. A restore that ignores the restore-and-resync constraints wedges synchronization.

## Owned paths

- **Implementer:** `core/rcdvlog-server/src/` (backup, restore, health services), `apps/server/src/` (health routes, backup triggers)
- **Independent tester:** `core/rcdvlog-server/tests/`, `apps/server/tests/`
- **Integrator:** `deploy/server/compose.yaml` (backup volume, schedule config)

## Required interface

- Backup and restore API for CLI and web admin
- Health/readiness endpoints with component status
- Log level configuration

## Acceptance oracle

1. Scheduled backup creates a verified database backup.
2. Backup verification detects corruption.
3. Restore from backup recovers server identity and project registries.
4. Post-restore sync reconciliation converges correctly.
5. Health/readiness reports component-level status accurately.
6. Log level changes take effect at runtime.
7. Disk pressure below threshold triggers graceful degradation.
8. Reseed/rescan path recovers derived media after disk loss.

## Independent test charter

Try to falsify: "backup restore is complete and safe." Take a verified backup,
corrupt a byte, verify integrity check fails. Wipe the control database, restore
from a clean backup, verify server identity, project registries, and sync cursors
are intact. Trigger sync post-restore and verify convergence. Verify health
endpoint reports each component's actual status, not a static "ok."

## Review charter

Risk domain: data durability and operational safety. Questions: (1) Can a
corrupted backup pass verification? (2) Does restore lose sync cursors? (3) Does
post-restore sync wedge? (4) Does health accurately reflect component status? (5)
Does a restore-and-resync constraint violation go undetected? Severity: P0 for
backup corruption passing verification, lost sync cursors, or wedged sync;
P1 for inaccurate health or missing restore constraints. Approval vocabulary:
`APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Incremental or differential backups
- Point-in-time recovery
- Off-site backup replication
- Backup of derived media (representations are regenerable)

## Stop conditions

Stop and escalate when:

- Backup requires stopping the server
- Restore cannot recover sync cursors from the backup
- Health endpoint mutates state or triggers side effects
- Disk pressure handling interrupts committed work

## Commands

```sh
cargo test -p rcdvlog-server
cargo test -p rcdvlog-server --test integration
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Backup/restore procedures are consumed by S01-T018 (CLI diagnostics), S01-T019
(acceptance), and S01-T020 (documentation). Restore-and-resync constraints must
be documented prominently.
