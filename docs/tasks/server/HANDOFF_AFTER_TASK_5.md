# Handoff After Task 5

**Date:** 2026-07-20 (post-review corrections applied)
**Status:** Task 5 implemented with post-review corrections; all automated tests pass; manual Ubuntu/NAS acceptance not run
**Epic:** [EPIC.md](EPIC.md)
**Next:** Task 6 — Shared Representations and Authenticated Media Delivery

## 1. Migration identity

Control catalog schema version: **4**

| ID | Migration | Immutable source identity |
| --- | --- | --- |
| 1 | `control_catalog_foundation` | `server-control/0001-control-catalog-foundation/20260720` |
| 2 | `storage_global_library` | `server-control/0002-storage-global-library/20260720` |
| 3 | `durable_server_job_engine` | `server-control/0003-durable-server-job-engine/20260720` |
| 4 | `nas_scan_verified_content_identity` | `server-control/0004-nas-scan-verified-content-identity/20260720` |

Migration 4 adds tables: `scan_runs`, `scan_run_jobs`, `scan_schedules`, `media_quick_fingerprint_facts`, `media_full_hash_facts`, `media_probe_facts`, `scan_file_diagnostics`, `media_mismatch_evidence`.

The upgrade test (`scan_migration_upgrade_preserves_existing_rows`) confirms migration 3→4 preserves existing server identity, project registry, storage roots, and job rows.

Migrations 1–3 are unchanged. No project-catalog migration was added (Task 5 does not introduce project membership).

## 2. Public API surface (new since Task 4)

### Types exported from `rcdvlog-server-core`

| Type | Module | Purpose |
| --- | --- | --- |
| `ScanRun` | `scan` | Scan state DTO |
| `ScanSchedule` | `scan` | Schedule DTO |
| `NewScanSchedule` | `scan` | Schedule creation input |
| `ScanScheduleUpdate` | `scan` | Schedule update input |
| `ScanSource` | `scan` | `manual` or `schedule` |
| `ScanState` | `scan` | `queued`, `running`, `succeeded`, `failed`, `canceled` |
| `ScanToolConfig` | `scan` | ffprobe/ExifTool executable paths |
| `ScanService` | `scan` | Scan/schedule domain service |
| `ScanServiceError` | `scan` | Error type |
| `ScanJobHandler` | `scan_handler` | Job handler dispatching all scan job types |
| `QuickFingerprintFact` | `media_facts` | Sampled BLAKE3 fact |
| `FullHashFact` | `media_facts` | Full BLAKE3 fact with nanosecond evidence |
| `ProbeFact` | `media_facts` | ffprobe + ExifTool fact |
| `ScanFileDiagnostic` | `media_facts` | Per-file error diagnostic |
| `MediaMismatchEvidence` | `media_facts` | Quick collision evidence |
| `build_root_scan_job` | `scan_handler` | Root job builder (for recovery) |
| `build_full_hash_job` | `scan_handler` | Full hash job builder |
| `build_scan_finalizer_job` | `scan_handler` | Finalizer job builder |
| `open_read_only_connection` | `catalog` | Read-only SQLite connection |
| `open_writable_connection` | `catalog` | Writable SQLite connection |
| `compute_next_run_utc` | `scan` | Schedule UTC calculation |
| `validate_local_time` | `scan` | HH:MM validation |
| `validate_timezone` | `scan` | IANA timezone validation |
| `safe_path_identity` | `scan` | Path→BLAKE3 diagnostic identifier |

### Key methods on `ScanService`

- `request_scan` — idempotently creates scan + root job
- `start_scan` — marks running
- `ensure_root_job_enqueued` — crash recovery
- `increment_scan_counters` / `increment_file_counters`
- `complete_scan` — terminal state
- `get_scan` / `list_scans` — query
- `create_schedule` / `update_schedule` / `get_schedule` / `list_schedules`
- `list_due_schedules` — schedule loop
- `record_schedule_trigger` — advance next-run
- `list_scan_diagnostics` — paginated file errors

### Key new methods on `JobEngine`

- `enqueue_with_id` — enqueue with pre-determined job ID

## 3. Job recipe keys

| Job type | Recipe key | Class |
| --- | --- | --- |
| `scan.root.v1` | `scan.root.discovery-v1` | lightweight |
| `scan.quick_fingerprint.v1` | `scan.quick-fingerprint.sampled-v1` | lightweight |
| `scan.media_probe.v1` | `scan.media-probe.ffprobe-v1` | lightweight |
| `scan.full_hash.v1` | `scan.full-hash.blake3-v1` | full_hash |
| `scan.finalizer.v1` | `scan.finalizer-v1` | lightweight |

## 4. Verified test counts

```
Total: 151 tests (all passing)
- 4 batch-estimator
- 7 CLI unit + 4 CLI process
- 27 core
- 3 desktop-library
- 5 server-adapter + 2 server-process
- 66 server-core lib
- 6 media_integration
- 1 public_api
- 26 scan_integration (12 strengthened/new)
```

Full gate (all passing):
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` (151 tests)
- `git diff --check`
- `pnpm check`

## 5. Known manual gaps

Not run and deliberately out of scope for this task:

- Ubuntu/Docker deployment with NFS/SMB mounts
- Sleep/wake, NAS disconnect/reconnect during scan
- Scale acceptance with thousands of 4K clips from a real NAS
- Resolve import, browser UI, project synchronization
- Memory/file-handle profiling during large scans
- Authentication, HTTPS, Tailscale

## 6. Task 6 starting constraints

Task 6 (Shared Representations and Authenticated Media Delivery) will need:

1. Representation recipe registration using the existing `media_objects` and full-hash identity from Task 5.
2. Representation generation jobs (proxy, thumbnail) as Task 4 durable jobs with new job types.
3. Atomic output commit to the shared derived-media root.
4. Authenticated media delivery routes over HTTPS.
5. It must NOT introduce project membership, synchronization, or unauthenticated routes.
6. Existing scanner and library tables are stable; add new Task 6 migrations, do not modify migration 4.
7. The derived-media root is configured separately from originals; representations must never touch original mounts.
8. Thumbnail and proxy recipes should match existing desktop versions (`proxy.h264-720p.orientation-v3`, `thumbnail.q1.orientation-v2`) unless a deliberate version bump is required.

## 7. Changes from Task 4 baseline

### core/rcdvlog-server/Cargo.toml
- Added `tracing` dependency

### core/rcdvlog-server/src/catalog.rs
- Migration 4: made `generation_id` NOT NULL, added `root_job_id` NOT NULL, added `scheduled_occurrence_at`, added `scan_run_jobs` table
- Made `open_read_only_connection` and `open_writable_connection` public
- Updated upgrade test

### core/rcdvlog-server/src/scan.rs
- `ScanRun`: added `generation_id: String` (not Option), `root_job_id: String`, `scheduled_occurrence_at: Option<String>`
- `request_scan`: generates IDs pre-insert, enqueues root job post-commit, idempotent retry
- `ensure_root_job_enqueued`: crash recovery
- `start_scan`: simplified (generation_id already set)
- Added `insert_scan_run_job`, `scan_children_settled`, `scan_child_outcomes`, `scan_children_settled_read_only`
- Added `set_scan_counters_absolute` for idempotent root retry
- Added `recover_active_scan_jobs` for startup recovery
- Added `RecoveryReport` DTO
- Updated `scan_from_row`, `load_scan`, `list_scans` for new columns

### core/rcdvlog-server/src/scan_handler.rs (rewritten)
- Added `build_root_scan_job` returning `Result` (no panics)
- `run_root_scan`: symlink-escape check via canonical paths, provisional observations, child job enqueue with `scan_run_jobs` tracking, idempotent counters via `set_scan_counters_absolute`
- `run_scan_finalizer`: read-only validation (no mutation); `commit_scan_finalizer` performs all publication atomically in transaction
- `commit_quick_fingerprint`: creates new provisional media object per storage (no auto-reattach)
- `commit_full_hash`: verified convergence, mismatch evidence recording, collision detection
- `commit_scan_finalizer`: full atomic publication — rechecks identifiers, generation state, child settlement, root success; completes generation; marks scan succeeded — all in one transaction
- All job builders return `Result<JobDefinition, String>` (no `.expect`)
- Payload version validation (v1 only)
- Unknown job type → terminal error
- Read-only connection for `has_current_quick_fact`
- Removed all `unwrap`/`expect` from production paths
- Diagnostic persistence failures → `tracing::warn!`

### core/rcdvlog-server/src/media_facts.rs
- Added `#[allow(dead_code)]` to transaction helpers kept for handler use

### core/rcdvlog-server/src/jobs.rs
- Added `enqueue_with_id` method
- `JobResultCommit` derives `Clone`

### core/rcdvlog-server/src/lib.rs
- Added public exports for `open_read_only_connection`, `open_writable_connection`
- Added `ScanJobHandler`, media facts types, scan helpers

### core/rcdvlog-server/tests/scan_integration.rs (new, 26 tests)
- 26 end-to-end tests driving `ScanService` + `JobEngine` + `ScanJobHandler`
- Includes tests for: atomic finalizer, ExifTool failure, changed-during-hash, convergence, rescan without deletion, schedule due + recovery, 2,000-row pagination, symlink escape, root job recovery, all-child-fail counters, migration preservation

### apps/server/src/main.rs
- Added startup scan recovery via `recover_active_scan_jobs()` before job scheduler
- Recovery logs structured counts/errors without exposing paths
- Schedule loop unchanged

### docs/
- `docs/tasks/server/NAS_SCAN_AND_CONTENT_IDENTITY.md` (new)
- `docs/tasks/server/HANDOFF_AFTER_TASK_5.md` (this file)
