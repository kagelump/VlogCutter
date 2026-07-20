# Companion Server Handoff After Task 4

**Date:** 2026-07-20
**Status:** Tasks 1–4 accepted; Task 5 not started
**Epic:** [EPIC.md](EPIC.md)
**Architecture:** [ARCHITECTURE.md](ARCHITECTURE.md)

## Current position

The companion-server foundation is implemented through the durable job engine. The repository now has versioned project and server-control migrations, stable cross-node identity types, a private Ubuntu server runtime, logical storage-root/global-library contracts, and a restart-safe SQLite job engine.

No scan, hash, probe, proxy, thumbnail, synchronization, authentication, or web-product handler has been implemented yet. Those boundaries are intentional: Task 5 is the next task.

The working tree is not committed or staged. It contains all Task 1–4 implementation and planning changes together with pre-existing repository changes. Preserve it, inspect `git status` before editing, and do not discard or broadly rewrite unrelated files.

## Accepted product decisions

- One user, one Mac, and one Ubuntu server are supported.
- Each desktop project has one origin-like server replica; the desktop remains an offline-capable checkout.
- The server owns a project-independent global media library and may scan configured NAS roots.
- Project annotations stay project-scoped even when projects share one media object.
- Sampled BLAKE3 plus size creates only a candidate identity. Cross-location convergence requires matching full-file BLAKE3 plus size.
- Shared media paths use a logical `storageRootId` and normalized root-relative path. Absolute paths stay node-local.
- The service must ultimately work through trusted LAN HTTPS at home and Tailscale HTTPS away; Chrome is the supported browser.
- NFS is preferred for the Ubuntu NAS mount when available, with SMB supported. Originals are read-only, derived media has a separate writable root, and all live SQLite state stays on the Ubuntu SSD.
- The final epic task, Task 20, updates repository-wide documentation after behavior and acceptance are complete.

## Completed work

### Task 1 — Schema and domain foundation

- Added numbered, contiguous, individually transactional SQLite migrations with immutable migration names and source identities.
- Added read-only schema inspection and explicit rejection of newer/incompatible schemas without mutating them.
- Added opaque validated IDs for nodes, projects, project assets, media objects, storage roots/objects, representations, operations, and jobs.
- Added project pairing fields and project-asset media-link verification fields without rewriting existing IDs or annotations.
- Added the shared canonical `RootRelativePath` type.

Focused contract: [SCHEMA.md](SCHEMA.md)

The project catalog is at schema version 4:

| ID | Migration | Immutable source identity |
| --- | --- | --- |
| 1 | `catalog_baseline` | `project-catalog/0001-catalog-baseline/20260720` |
| 2 | `legacy_orientation_compatibility` | `project-catalog/0002-legacy-orientation-compatibility/20260720` |
| 3 | `legacy_annotation_compatibility` | `project-catalog/0003-legacy-annotation-compatibility/20260720` |
| 4 | `companion_identity_links` | `project-catalog/0004-companion-identity-links/20260720` |

Do not rename, reorder, edit, or reuse these migration identities.

### Task 2 — Server workspace and runtime

- Added `core/rcdvlog-server` for server domain/runtime foundation and `apps/server` as the thin Axum/Tokio process adapter.
- Added a stable server node identity, local-SSD control catalog, advisory single-process lock, structured/redacted configuration and logging, loopback-only listener validation, health/readiness/version routes, and bounded graceful shutdown.
- Startup checks required tools and declared paths without preventing liveness when external media paths or tools are unavailable.
- Added a Linux Dockerfile and Compose model using host networking so the container listener remains Ubuntu-loopback-only. Original media is mounted read-only and control state uses a local Docker volume.

Focused contract: [SERVER_RUNTIME.md](SERVER_RUNTIME.md)

The server currently exposes only private foundation routes on `127.0.0.1:8090` by default. Product APIs must not be exposed directly on LAN or Tailscale before Task 9 adds authenticated HTTPS front doors.

### Task 3 — Storage roots and global media library

- Added logical storage roots, node-local bindings, path-free remote capabilities, media/storage identities, project-membership projections, scoped observation generations, availability projections, and cursor pagination.
- Kept project annotations out of the server global library. Project catalogs remain authoritative for project membership and annotations.
- Added explicit, confirmed, verified root-binding requirements and canonical symlink-aware containment checks.
- Enforced read-only resolution for immutable original roots before filesystem access.
- Added node- and scope-aware observation publication. In-progress or abandoned scans cannot make older files appear missing.
- Kept absolute mount paths out of synchronization-safe DTOs and remote capabilities.

Focused contract: [STORAGE_LIBRARY.md](STORAGE_LIBRARY.md)

### Task 4 — Durable server job engine

- Added durable jobs, attempts, dependencies, leases, retry policy, cancellation, weighted progress, rate history, and replayable events.
- Added atomic claim and lease-token-guarded heartbeat/progress/fail/complete operations.
- Added bounded versioned payloads and immutable input identities with idempotency hashing. Command-shaped payloads, secrets, and unrestricted path details are rejected.
- Added closed typed requirements/capabilities and optional typed project, media, root, and representation IDs.
- Added transactional result registration through `JobResultCommit`: handler output registration and terminal job success share one SQLite transaction.
- Added structured retryable/terminal handler failures and capped retry/backoff behavior.
- Integrated `batch-estimator` with actual worker count and individual remaining task weights. Only successful item observations update throughput history; failed items advance progress without poisoning rate history.
- Added configurable class caps: proxy 0–1, full hash 0–1, and lightweight 0–2, with at least one total worker.
- Added responsive durable cancellation, lease-loss cancellation, graceful shutdown/draining rules, and a generic graceful-then-forced child-process cancellation contract.
- Added startup recovery for expired leases. Recovered jobs persist a safe structured `lease_expired` error; retryable recovery remains `retry_wait` until the next claim promotes it.
- Added read-only `JobEngine::get_job` and safe `JobSnapshot`/`JobSnapshotError` monitoring. Snapshots expose lifecycle/progress/error/output identity but never lease owner or token, and remain readable during another WAL writer.
- Added successful-output reuse through idempotent enqueue plus snapshot lookup.

Focused contract: [JOB_ENGINE.md](JOB_ENGINE.md)

The control catalog is at schema version 3:

| ID | Migration | Immutable source identity |
| --- | --- | --- |
| 1 | `control_catalog_foundation` | `server-control/0001-control-catalog-foundation/20260720` |
| 2 | `storage_global_library` | `server-control/0002-storage-global-library/20260720` |
| 3 | `durable_server_job_engine` | `server-control/0003-durable-server-job-engine/20260720` |

Do not modify migrations 1–3. Task 5 must add a forward control-catalog migration if it needs scan schedules, checkpoints, or immutable media facts.

## Important implementation map

| Area | Files |
| --- | --- |
| Project IDs, paths, migrations, and catalog integration | `core/rcdvlog-core/src/identifiers.rs`, `storage.rs`, `migrations.rs`, `lib.rs` |
| Server control migrations and stable node identity | `core/rcdvlog-server/src/catalog.rs` |
| Server configuration and readiness | `core/rcdvlog-server/src/config.rs`, `foundation.rs` |
| Logical roots and global library | `core/rcdvlog-server/src/storage_path.rs`, `library.rs` |
| Durable jobs and scheduler | `core/rcdvlog-server/src/jobs.rs` |
| Public server-core exports | `core/rcdvlog-server/src/lib.rs`, `core/rcdvlog-server/tests/public_api.rs` |
| Thin HTTP/process adapter | `apps/server/src/lib.rs`, `apps/server/src/main.rs` |
| Server process acceptance | `apps/server/tests/server_process.rs` |
| Deployment foundation | `deploy/server/Dockerfile`, `deploy/server/compose.yaml`, `deploy/server/example.env` |
| Weighted ETA extension | `core/batch-estimator/src/lib.rs` |

## Verified state at acceptance

The primary agent independently ran these gates after the final recovered-lease snapshot fix:

```text
pnpm check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

All passed. `pnpm check` completed the desktop TypeScript build check and 108 Rust tests:

- 4 batch-estimator tests
- 7 CLI unit tests and 4 CLI process/media tests
- 27 core tests
- 3 desktop-library tests
- 5 server-adapter tests and 2 server-process tests
- 55 server-core tests
- 1 external server job API test

No real Ubuntu, Docker-on-Ubuntu, NFS, SMB, NAS-scale, LAN HTTPS, Tailscale, or Chrome acceptance was run. Scanning and product network surfaces do not exist yet, so those checks belong to later tasks.

## Task 5 starting point

Next plan: [TASK_5_nas_scan_and_verified_content_identity.md](TASK_5_nas_scan_and_verified_content_identity.md)

Task 5 implements manual and persisted scheduled scans, media probing, sampled quick fingerprints, full-file BLAKE3 verification, and reconciliation into the existing global library. Its key boundaries are:

1. Resolve scan scopes only through `StorageLibrary` and the current server's confirmed local root binding. Never accept an arbitrary absolute API path.
2. Walk configured scopes without following symlinks, and revalidate canonical containment before every file job.
3. Keep original roots read-only and place no temp files, markers, logs, or derived output beside originals.
4. Keep supported extensions aligned with the Rust core.
5. Treat sampled BLAKE3 plus size as provisional. Merge different storage objects only after full BLAKE3 plus size matches.
6. Record size and modification evidence before and after full hashing. Return retryable `input_changed` instead of committing an identity when the file changes during the read.
7. Register scan, probe, quick-hash, and full-hash work as versioned Task 4 jobs. Use typed handlers, structured failures, bounded safe details, real work weights, cancellation, and transactional `commit_result` registration.
8. Persist observation generations incrementally, but publish missing/unavailable conclusions only after a complete successful scope scan. Interrupted generations must not hide previously known files.
9. Keep ffprobe as the technical metadata source and make ExifTool enrichment gracefully optional.
10. Keep global media facts separate from project annotations and do not create project membership automatically.
11. Add real-media and collision/change-during-read tests only against temporary fixtures; never run automated scans against `/Volumes` or a real NAS.
12. Do not start Task 6 until Task 5 passes its focused and full review gates.

No new product question blocks Task 5. Actual Ubuntu mount paths/protocol and later HTTPS names can be selected during deployment work without changing the accepted logical-root architecture.

## Continuation workflow

The user requested sequential implementation. Delegate exactly one epic task at a time to a `gpt-5.6-terra` subagent with high reasoning, then independently review its diff, contracts, tests, documentation, and full gates before starting the next task. Do not parallelize Tasks 5–20 or treat a subagent's green report as acceptance without primary-agent review.

Task 20 remains the broad documentation pass. Until then, focused task handoffs should accurately describe implemented behavior without presenting later epic capabilities as complete.
