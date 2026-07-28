# S01: Local-First Companion Server

<!-- bounded-epic-metadata
{
  "schema_version": 1,
  "epic_id": "S01",
  "title": "Local-First Companion Server",
  "base_branch": "main",
  "task_branch_template": "codex/{task_id_lower}-{slug}",
  "tasks_per_cycle": 1,
  "merge_requires_user_authorization": true
}
-->

## Outcome

Deliver a companion server that acts as the durable origin-like replica for each
desktop project, indexes immutable NAS media into a reusable global library, runs
restart-safe media jobs, synchronizes current project state without blocking
offline desktop work, and serves a secure Chrome review application over both the
home LAN and Tailscale.

The completed epic must feel like one project appearing on two surfaces. The
server is not a required backend for desktop saves, and the desktop is not a
remote control for a server-owned project.

## Frozen context

- Product contract: `docs/tasks/server/ARCHITECTURE.md`
- Architecture contract: `docs/tasks/server/ARCHITECTURE.md`
- Storage model: `docs/tasks/server/STORAGE_LIBRARY.md`
- Schema: `docs/tasks/server/SCHEMA.md`
- Operations: `docs/tasks/server/OPERATIONS.md`

### Architectural constraints

- Originals are immutable and server original mounts are read-only.
- SQLite is canonical local state on each node, and live databases stay on local SSDs.
- SQLite files and SQLite changesets are not the synchronization protocol.
- Rust domain commands are shared by desktop, server, web adapters, and CLI.
- The desktop never waits for a network round trip to commit an ordinary local edit.
- The server never connects inbound to the Mac.
- The browser is online-only and talks only to the server.
- Absolute paths are node-local. Protocol state uses logical root IDs and relative paths.
- Cross-location content merges require verified full-file hashes.
- A representation is immutable and identified by verified input plus versioned recipe.
- Network retries, scan retries, job retries, and baseline retries are idempotent.
- Existing catalogs migrate forward; deletion and rebuild are not migration strategies.
- Read-only diagnostics must not migrate, scan, hash, generate media, or repair state.

## Queue

| Order | Task | Title | Depends on | Status |
| ---: | --- | --- | --- | --- |
| 10 | [S01-T001](tasks/S01-T001-schema-and-domain-foundation.md) | Schema and domain foundation | — | complete |
| 20 | [S01-T002](tasks/S01-T002-server-workspace-and-runtime.md) | Server workspace and runtime | S01-T001 | complete |
| 30 | [S01-T003](tasks/S01-T003-storage-roots-and-global-media-library.md) | Storage roots and global media library | S01-T001, S01-T002 | complete |
| 40 | [S01-T004](tasks/S01-T004-durable-server-job-engine.md) | Durable server job engine | S01-T002 | complete |
| 50 | [S01-T005](tasks/S01-T005-nas-scan-and-verified-content-identity.md) | NAS scanning and verified content identity | S01-T003, S01-T004 | complete |
| 60 | [S01-T006](tasks/S01-T006-representation-store-media-jobs-and-streaming.md) | Representation store, media jobs, and streaming | S01-T004, S01-T005 | complete |
| 70 | [S01-T007](tasks/S01-T007-operation-backed-project-mutations.md) | Operation-backed project mutations | S01-T001 | complete |
| 80 | [S01-T008](tasks/S01-T008-sync-protocol-and-server-project-replicas.md) | Sync protocol and server project replicas | S01-T002, S01-T003, S01-T007 | complete |
| 90 | [S01-T009](tasks/S01-T009-authentication-and-secure-transports.md) | Authentication and secure LAN/Tailscale transport | S01-T002 | complete |
| 100 | [S01-T010](tasks/S01-T010-desktop-pairing-root-mapping-and-baseline.md) | Desktop pairing, root mapping, and baseline adoption | S01-T008, S01-T009 | complete |
| 110 | [S01-T011](tasks/S01-T011-automatic-sync-and-conflict-handling.md) | Automatic incremental sync and conflict handling | S01-T008, S01-T010 | complete |
| 120 | [S01-T012](tasks/S01-T012-remote-representation-exchange-and-mac-requests.md) | Remote representation exchange and Mac-targeted requests | S01-T006, S01-T011 | complete |
| 130 | [S01-T013](tasks/S01-T013-web-foundation-typed-client-and-shared-ui.md) | Web foundation, typed client, and shared UI seam | S01-T009 | complete |
| 140 | [S01-T014](tasks/S01-T014-web-library-project-browse-and-add.md) | Library/project browsing and add-to-project | S01-T005, S01-T006, S01-T008, S01-T013 | complete |
| 150 | [S01-T015](tasks/S01-T015-web-proxy-review-and-annotations.md) | Proxy review and annotation editing | S01-T006, S01-T007, S01-T011, S01-T013, S01-T014 | complete |
| 160 | [S01-T016](tasks/S01-T016-web-jobs-storage-sync-and-credential-admin.md) | Jobs, storage, sync, credential, and schedule administration | S01-T003, S01-T004, S01-T005, S01-T006, S01-T009, S01-T011, S01-T013 | complete |
| 170 | [S01-T017](tasks/S01-T017-backup-restore-health-and-operational-hardening.md) | Backup, restore, health, and operational hardening | S01-T002, S01-T004, S01-T008, S01-T009 | complete |
| 180 | [S01-T018](tasks/S01-T018-cli-and-machine-readable-diagnostics.md) | CLI and machine-readable diagnostics | S01-T005, S01-T006, S01-T008, S01-T009, S01-T011, S01-T017 | complete |
| 190 | [S01-T019](tasks/S01-T019-end-to-end-security-scale-and-deployment-acceptance.md) | End-to-end, security, scale, and deployment acceptance | S01-T012, S01-T015, S01-T016, S01-T017, S01-T018 | complete |
| 200 | [S01-T020](tasks/S01-T020-update-all-documentation.md) | Repository-wide documentation update | S01-T019 | complete |

## Promotion rule

The epic is promoted only when every required implementation, QA, review, and
release-gate packet is complete. One task cycle never promotes unrelated work.

Release-topology acceptance is still outstanding until the 17-row
Ubuntu/Mac/NAS/LAN/Tailscale matrix in `ACCEPTANCE_TASK_19.md` passes. A browser
mock, an unverified migration, or local automation alone is not release approval.

## Cross-cutting verification

```bash
pnpm --dir apps/desktop build
pnpm --dir apps/web build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm test:integration
pnpm test:web:e2e
docker compose -f deploy/server/compose.yaml config
```

## Follow-on

The [Travel Ingest epic](../travel/EPIC.md) extends this work with phone-carried
SD-card ingest, a writable NAS archive root, a resumable original-media transfer
protocol, and server-origin project creation.
