# S01-T002: Server Workspace and Runtime

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T002",
  "epic_id": "S01",
  "title": "Server Workspace and Runtime",
  "slug": "server-workspace-and-runtime",
  "status": "complete",
  "task_class": "foundation",
  "depends_on": [
    "S01-T001"
  ],
  "principal_invariant": "The server process starts, reports health, preserves its identity and configuration on local SSD, and shuts down cleanly — and two processes cannot silently act as the same server against one control database.",
  "central_falsification": "Start the server, record its nodeId, kill it, restart it, and assert the nodeId is unchanged. Then attempt to start a second process against the same control database and assert it exits with a lock/lease error without corrupting the database.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Server binary → control catalog",
    "Server binary → HTTP listener"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/SERVER_RUNTIME.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/SERVER_RUNTIME.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/SERVER_RUNTIME.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/SERVER_RUNTIME.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/OPERATIONS.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T002-server-workspace-and-runtime.md"
    ],
    "implementer": [
      "core/rcdvlog-server/src/**",
      "apps/server/src/**",
      "deploy/server/Dockerfile",
      "deploy/server/compose.yaml",
      "deploy/server/example.env"
    ],
    "independent_tester": [
      "core/rcdvlog-server/tests/**",
      "apps/server/tests/**"
    ],
    "integrator": [
      "Cargo.toml",
      "Cargo.lock",
      "package.json"
    ]
  },
  "forbidden_paths": [
    "core/rcdvlog-core/src/**",
    "core/rcdvlog-sync/**",
    "apps/desktop/**",
    "apps/web/**",
    "apps/cli/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server-core",
    "cargo test -p rcdvlog-server",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings",
    "docker compose -f deploy/server/compose.yaml config"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 20 \\| \\[S01-T002\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Add the Ubuntu-compatible Rust server process, server-domain crate, local control
catalog, configuration model, and safe development deployment skeleton.

## Principal invariant

This task is correct when the server process starts, reports health, preserves
its identity and configuration on local SSD, and shuts down cleanly — and two
processes cannot silently act as the same server against one control database.

## Inputs and outputs

Inputs:

- Task 1 identity types and migration framework
- Workspace Cargo.toml layout

Outputs:

- `apps/server` Rust binary with structured logging, health/readiness endpoints, graceful shutdown
- `core/rcdvlog-server` crate for server-specific domain and persistence
- Server control SQLite database with formal migrations and stable `nodeId`
- Typed configuration with validation, defaults, redacted diagnostics
- Dockerfile and Docker Compose layout using local-SSD state

## Frozen decisions

1. Server targets Ubuntu amd64; runs without macOS/Tauri dependencies.
2. Domain/persistence in `core/rcdvlog-server`; HTTP/runtime wiring thin in `apps/server`.
3. Default data directory `/var/lib/rcdvlog` in deployment, temporary path in tests.
4. Control database uses WAL on local storage only, same migration discipline as project catalogs.
5. Startup validates directory containment, ownership, schema, tool availability, and configuration before reporting ready.
6. Liveness = event loop running. Readiness = control catalog usable + config valid.
7. Secrets use redacted wrapper types, never emitted in logs or diagnostics.
8. Initial listener is loopback-only — must not publish unauthenticated service to LAN.
9. Configuration errors use stable codes and name the invalid field without leaking secrets.
10. Built binary reports application, protocol, and control-schema versions.

## Owned paths

- **Implementer:** `core/rcdvlog-server/`, `apps/server/`, `deploy/server/Dockerfile`, `deploy/server/compose.yaml`, `deploy/server/example.env`
- **Independent tester:** `core/rcdvlog-server/tests/`, `apps/server/tests/`
- **Integrator:** workspace `Cargo.toml`, `Cargo.lock`, root `package.json` command aliases only

## Required interface

- Internal HTTP listener contract (loopback, health/readiness routes)
- Control-catalog migration API for downstream tasks
- Server identity and configuration types in `rcdvlog-server`

## Acceptance oracle

1. First start creates data directory, control database, schema history, and stable server node identity.
2. Restart returns same server node identity.
3. Second process against same control database exits with actionable lock/lease error.
4. `/api/v1/health` reports liveness without mutating state.
5. `/api/v1/ready` distinguishes ready from invalid config or unavailable local state.
6. SIGTERM performs bounded graceful shutdown.
7. JSON logs include timestamp, level, component, request ID, and redacted errors.
8. Compose stores `/var/lib/rcdvlog` on explicit persistent volume/host path, not on NAS.
9. Initial service reachable only from loopback in default Compose configuration.
10. No original or derived directories modified by startup checks.

## Independent test charter

Try to falsify: "the server preserves identity across restarts and rejects
duplicate processes." Start server, record nodeId, SIGTERM, restart, verify
nodeId unchanged. Start second process pointing at same data dir, verify it exits
with error and first process's database is uncorrupted. Use temporary data
directories distinct from the implementer's test fixtures.

## Review charter

Risk domain: server identity and data safety. Questions: (1) Can nodeId change
across restarts? (2) Can two servers share a control database without error? (3)
Does any startup check write to original or derived directories? (4) Is the
listener accidentally reachable from non-loopback? (5) Are secrets ever emitted
in logs? Severity: P0 for data corruption or accidental LAN exposure; P1 for
missing graceful shutdown or leaked configuration. Approval vocabulary:
`APPROVE`, `APPROVE_WITH_FOLLOW_UP`, `REJECT`.

## Non-goals

- LAN or Tailscale exposure
- Authentication
- NAS scanning and storage-root records
- Project replication or sync endpoints
- Durable jobs
- Production backup behavior
- Web application

## Stop conditions

Stop and escalate when:

- The server requires macOS/Tauri dependencies to compile
- Configuration or logging leaks secret values
- The listener binds to non-loopback without explicit authorization
- Control-catalog schema diverges from the project-catalog migration discipline established in Task 1

## Commands

```sh
cargo test -p rcdvlog-server-core
cargo test -p rcdvlog-server
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
docker compose -f deploy/server/compose.yaml config
```

## Handoff

Record the internal listener contract and control-database migration API for
Tasks 3, 4, 8, 9, and 17. The service remains intentionally private and
incomplete until Task 9.
