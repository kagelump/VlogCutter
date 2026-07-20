# Task 2: Server Workspace and Runtime

**Status:** Planned
**Depends on:** Task 1
**Unblocks:** Tasks 3, 4, 8, 9, and 17

## Objective

Add the Ubuntu-compatible Rust server process, server-domain crate, local control catalog, configuration model, and safe development deployment skeleton.

## User outcome

The companion server can be installed and started on the Ubuntu laptop, reports health, preserves its identity and configuration on local SSD, and shuts down cleanly. It does not yet scan media or expose an authenticated product UI.

## Scope

- Add `apps/server` Rust binary.
- Add `core/rcdvlog-server` for server-specific domain and persistence behavior.
- Add a server control SQLite database with formal migrations.
- Generate and persist one stable server `nodeId`.
- Implement typed configuration loading, validation, defaults, and redacted diagnostics.
- Add structured JSON logging, request IDs, liveness, readiness, and graceful shutdown.
- Add a minimal static/version response and internal HTTP listener.
- Add initial Dockerfile and Docker Compose layout using local-SSD state.
- Add configuration for data, temporary, derived, backup, and allowed mount paths without yet scanning them.

## Out of scope

- LAN or Tailscale exposure
- Authentication
- NAS scanning and storage-root records
- Project replication or sync endpoints
- Durable jobs
- Production backup behavior
- Web application

## Design requirements

1. The server targets Ubuntu amd64 and runs without macOS/Tauri dependencies.
2. Domain and persistence code lives in `core/rcdvlog-server`; HTTP/runtime wiring stays thin in `apps/server`.
3. The default data directory is `/var/lib/rcdvlog` in deployment and a deliberate temporary/development path in tests.
4. The control database uses WAL only on local storage and the same numbered migration discipline as project catalogs.
5. Startup validates directory containment, ownership/access, database schema, tool availability, and configuration before reporting ready.
6. Liveness means the process event loop is running. Readiness means the control catalog is usable and required configuration is valid.
7. Graceful shutdown stops accepting new requests, signals workers through a future-compatible cancellation primitive, checkpoints connections normally, and exits within a bounded timeout.
8. Secrets are represented by redacted wrapper types and never emitted in logs or config diagnostics.
9. The initial listener is loopback-only. This task must not accidentally publish an unauthenticated service to the LAN.
10. The built binary reports application, protocol, and control-schema versions.
11. Configuration errors use stable codes and name the invalid field without leaking secret values.

## Initial control-catalog responsibilities

Create only the foundation required by later migrations:

- Server identity
- Configuration revision metadata
- Project registry shell
- Migration history
- Operational timestamps

Storage, library, authentication, job, and backup tables belong to their owning tasks.

## Likely implementation areas

- `Cargo.toml` workspace members and dependencies
- `core/rcdvlog-server/`
- `apps/server/`
- `deploy/server/Dockerfile`
- `deploy/server/compose.yaml`
- `deploy/server/example.env`
- Root `package.json` command aliases only where useful

## Acceptance criteria

- [ ] `rcdvlog-server` builds and tests on macOS development hosts and Ubuntu amd64 CI/runtime.
- [ ] First start creates the data directory, control database, schema history, and stable server node identity.
- [ ] Restart returns the same server node identity.
- [ ] Two processes cannot silently act as the same server against one control database; the second exits with an actionable lock/lease error.
- [ ] `/api/v1/health` reports liveness without mutating state.
- [ ] `/api/v1/ready` distinguishes ready from invalid configuration or unavailable local state.
- [ ] SIGTERM performs a bounded graceful shutdown.
- [ ] JSON logs include timestamp, level, component, request ID where applicable, and redacted errors.
- [ ] Compose stores `/var/lib/rcdvlog` in an explicit persistent volume or host path and does not place it on a NAS mount.
- [ ] The initial service is reachable only from loopback in the default Compose configuration.
- [ ] No original or derived directories are modified by startup checks.

## Verification

Run:

```bash
cargo test -p rcdvlog-server-core
cargo test -p rcdvlog-server
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
docker compose -f deploy/server/compose.yaml config
```

Smoke-test start, readiness, SIGTERM, restart, and stable node identity using a temporary data directory.

## Handoff notes

Record the internal listener contract and control-database migration API for Tasks 3, 4, 8, 9, and 17. The service remains intentionally private and incomplete until Task 9.
