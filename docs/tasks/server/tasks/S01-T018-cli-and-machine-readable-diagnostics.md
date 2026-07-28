# S01-T018: CLI and Machine-Readable Diagnostics

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T018",
  "epic_id": "S01",
  "title": "CLI and Machine-Readable Diagnostics",
  "slug": "cli-and-machine-readable-diagnostics",
  "status": "complete",
  "task_class": "implementation",
  "depends_on": [
    "S01-T005",
    "S01-T006",
    "S01-T008",
    "S01-T009",
    "S01-T011",
    "S01-T017"
  ],
  "principal_invariant": "Every server diagnostic command returns stable JSON, never mutates state, and read-only commands never migrate, scan, hash, generate media, or repair state.",
  "central_falsification": "Run every diagnostic command against a running server, verify each returns valid JSON with stable field names. Run the 'doctor' command and verify it does not create schema_migrations, trigger a scan, or generate a representation. Run a read-only catalog inspection on an already-upgraded catalog and verify zero writes.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "CLI → server HTTP API (S01-T009 auth)"
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
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T018-cli-and-machine-readable-diagnostics.md"
    ],
    "implementer": [
      "apps/cli/src/**"
    ],
    "independent_tester": [
      "apps/cli/tests/**"
    ],
    "integrator": []
  },
  "forbidden_paths": [
    "apps/desktop/**",
    "apps/web/src/**",
    "core/rcdvlog-server/src/transfer.rs"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-cli",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 180 \\| \\[S01-T018\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Add server-targeted CLI commands with stable JSON output for diagnostics,
administration, and inspection — every command read-only where promised, never
mutating state or triggering side effects.

## Principal invariant

This task is correct when every server diagnostic command returns stable JSON,
never mutates state, and read-only commands never migrate, scan, hash, generate
media, or repair state.

## Inputs and outputs

Inputs:
- S01-T005 media library and scan infrastructure
- S01-T006 representation store
- S01-T008 sync status
- S01-T009 authentication
- S01-T011 sync diagnostics
- S01-T017 backup and health

Outputs:
- Server status and health (JSON)
- Library query and inspection (read-only, paginated)
- Storage root and binding diagnostics
- Job queue inspection and management
- Sync status per project
- Backup trigger and verification
- Credential listing
- Strict read-only guarantee enforced at the command level

## Frozen decisions

1. All output is stable JSON with documented schema.
2. Read-only commands never migrate, scan, hash, generate media, or repair state.
3. Commands use stable error codes and machine-readable contracts.
4. Secrets and absolute paths are never exposed in output.
5. CLI commands authenticate to the server using the same device credentials.

## Owned paths

- **Implementer:** `apps/cli/` (all server diagnostic and admin commands)
- **Independent tester:** `apps/cli/tests/` with a running server
- **Integrator:** no new integration paths

## Required interface

- All server subsystem APIs consumed by CLI commands
- Stable JSON output schemas for each command

## Acceptance oracle

1. Every diagnostic command returns valid JSON with stable field names.
2. Read-only commands produce zero writes to any database.
3. `doctor` does not create schema_migrations, trigger scans, or generate media.
4. Catalog inspection on already-upgraded catalog produces zero writes.
5. Secrets and absolute private paths never appear in output.
6. Error codes are stable and machine-readable.
7. Commands authenticate with device credentials.

## Independent test charter

Try to falsify: "read-only commands never mutate." Run every read-only command
against a server with known state, capture a full database checksum before and
after each command, and assert zero changes. Run `doctor` against a healthy
server, verify no schema_migrations rows, scan jobs, or representation jobs are
created. Run catalog inspection on an already-upgraded catalog and verify zero
writes to the file.

## Review charter

Risk domain: diagnostic safety. Questions: (1) Can a read-only command mutate
state? (2) Is JSON output stable? (3) Are secrets exposed? (4) Are error codes
stable? Severity: P0 for read-only commands that mutate state or expose secrets;
P1 for unstable JSON schemas or error codes. Approval vocabulary: `APPROVE`,
`APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- Interactive CLI or TUI
- Desktop-targeted commands (existing CLI unchanged)
- Server configuration editing through CLI
- Historical data or trend analysis

## Stop conditions

Stop and escalate when:

- A read-only command needs to write to complete its function
- JSON output contains absolute paths or secrets
- A command's output schema changes based on server state
- CLI needs a separate authentication mechanism from the REST API

## Commands

```sh
cargo test -p rcdvlog-cli
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

CLI diagnostics complete the operational tooling surface. Consumed by S01-T019
(acceptance) and S01-T020 (documentation).
