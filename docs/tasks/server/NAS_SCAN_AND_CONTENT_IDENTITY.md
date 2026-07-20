# NAS Scan and Verified Content Identity — Implementation Contract

**Status:** Implemented, automated tests pass
**Date:** 2026-07-20
**Epic task:** TASK_5_nas_scan_and_verified_content_identity
**Parent:** docs/tasks/server/TASK_5_nas_scan_and_verified_content_identity.md

## 1. Schema (Migration 4)

Control catalog migration 4 adds:

| Table | Purpose |
| --- | --- |
| `scan_runs` | Durable scan state with pre-generated `generation_id` and `root_job_id` |
| `scan_run_jobs` | Normalized child-job tracking for finalizer settlement |
| `scan_schedules` | Disabled-by-default daily schedules with IANA timezone and next-run UTC |
| `media_quick_fingerprint_facts` | Versioned sampled BLAKE3 facts per storage object |
| `media_full_hash_facts` | Versioned full BLAKE3 facts with pre/post read nanosecond evidence |
| `media_probe_facts` | Versioned ffprobe facts with optional ExifTool enrichment status |
| `scan_file_diagnostics` | Bounded per-file errors; no absolute paths, no raw stderr |
| `media_mismatch_evidence` | Quick-fingerprint collision evidence linking candidates |

### Key design decisions

- `scan_runs.generation_id` and `scan_runs.root_job_id` are NOT NULL; both are generated before the scan row is inserted.
- `scan_runs.scheduled_occurrence_at` provides idempotency for schedule-triggered scans: `scheduleId` + UTC occurrence.
- `scan_run_jobs` normalizes child-job linkage so the finalizer does not hold thousands of dependency IDs in `JobDefinition.dependencies`.
- Modification timestamps use `i64` nanoseconds on Unix (via `std::os::unix::fs::MetadataExt`) for high-resolution round-trip precision.

## 2. Scan lifecycle

### 2.1 Request

`ScanService::request_scan`:
1. Validates the storage root exists.
2. Checks for an existing active scan for the same (node, root, scope) → returns it idempotently after ensuring its root job is durable.
3. Generates stable `scan_id`, `generation_id`, and `root_job_id` from `Uuid::new_v4()`.
4. Inserts the scan row with all IDs and `scheduled_occurrence_at` (if schedule-sourced).
5. Enqueues the root scan job via `JobEngine::enqueue_with_id` using `build_root_scan_job`.

If step 5 fails, the scan row remains queued and the error is returned. A future retry or startup recovery can re-enqueue the root job.

### 2.2 Discovery (root handler)

`ScanJobHandler::run_root_scan`:
1. Validates the canonical scan root is within the confirmed binding.
2. Walks using `WalkDir::follow_links(false)`.
3. For each supported media file:
   - Converts to NFC `RootRelativePath` relative to the canonical binding.
   - Upserts/finds a `StorageObject`.
   - Records a provisional `accessible` observation (in-progress generation).
   - If size+mtime+quick-fact match disk, skips as unchanged.
   - Otherwise enqueues quick-fingerprint, probe, and full-hash child jobs.
   - Records each child job in `scan_run_jobs`.
4. After discovery, enqueues a finalizer job and records it + the root job in `scan_run_jobs`.

### 2.3 Child handlers

- **Quick fingerprint:** Re-resolves, samples BLAKE3, checks pre/post evidence. On `input_changed`, returns retryable. On commit: creates a new provisional `MediaObject` + location (does NOT reattach to an existing media object based only on quick equality).
- **Probe:** Runs ffprobe + optional ExifTool. ExifTool failure stores ffprobe data + enrichment warning.
- **Full hash:** Streams BLAKE3 with cancellation. On `input_changed`, returns retryable. On commit:
  - If a verified `(fullHash, size)` match exists → relinks this storage to that verified media object.
  - Otherwise → promotes the current provisional media object to verified.
  - If a quick-fingerprint collision with a different full hash exists → records `quick_fingerprint_collision` mismatch evidence; keeps both media objects distinct.

### 2.4 Finalization (atomic)

`ScanJobHandler::run_scan_finalizer` is **read-only**:
1. Opens a read-only connection.
2. Checks `scan_children_settled_read_only` (no child jobs in queued/running/retry_wait).
3. If not settled → returns retryable `scan_not_settled`.
4. Validates scan is `running` and generation is `in_progress`.
5. Returns a result; does NOT mutate `observation_generations` or `scan_runs`.

All mutation occurs atomically in `commit_scan_finalizer`, inside the same transaction
that marks the finalizer job succeeded (via `JobEngine::complete`):
1. Re-checks that scan/generation/root identifiers match.
2. Verifies generation is `in_progress` (or already `completed` idempotently).
3. Re-checks no child job is queued/running/retry_wait.
4. Verifies root discovery job is durably succeeded.
5. Computes `completed_files` (settled distinct storage objects) and `failed_files` (subset with terminal failures).
6. Completes the observation generation.
7. Updates `scan_runs` state to `succeeded` with counters.

If the transaction fails or is rolled back, neither the generation nor the scan
are published — satisfying the atomic publication contract from Task 4.

The finalizer has no explicit `dependencies` on `JobDefinition` (avoiding thousands
of dependency IDs). Settlement is checked via `scan_run_jobs JOIN jobs`.

### 2.5 Cancellation and failure

- Cancellation during root scan → abandons generation, marks scan canceled.
- Per-file failures are durable diagnostics in `scan_file_diagnostics`; they do not abort unrelated files.
- A bad file whose jobs all fail terminally → counted as `failed_files`; the scan can still succeed.
- Terminal root binding/traversal failure → marks scan failed, abandons generation.

## 3. Job recipes

| Job type | Recipe key | Class | Payload version |
| --- | --- | --- | --- |
| `scan.root.v1` | `scan.root.discovery-v1` | lightweight | 1 |
| `scan.quick_fingerprint.v1` | `scan.quick-fingerprint.sampled-v1` | lightweight | 1 |
| `scan.media_probe.v1` | `scan.media-probe.ffprobe-v1` | lightweight | 1 |
| `scan.full_hash.v1` | `scan.full-hash.blake3-v1` | full_hash | 1 |
| `scan.finalizer.v1` | `scan.finalizer-v1` | lightweight | 1 |

All handlers reject unknown job types and unsupported payload versions with terminal errors.

## 4. Identity rules

1. Sampled BLAKE3 + size is provisional evidence only. A new storage object always creates its own provisional media object; it is never auto-attached to an existing media object based on quick fingerprint alone.
2. Full BLAKE3 + size is the only cross-location convergence step. If a verified match exists, the storage location is relinked to that verified media object.
3. Quick collision + full mismatch → both media objects remain distinct; `quick_fingerprint_collision` mismatch evidence is recorded.
4. Old media objects are preserved when they retain another location or project membership. No historical facts are deleted.

## 5. Schedule semantics

- Daily schedules with `HH:MM` local time and IANA timezone.
- `compute_next_run_utc` finds the next UTC instant ≥ now.
- Disabled schedules are never enqueued.
- Schedule-triggered scans use `scheduleId | UTC occurrence` for idempotency.
- After triggering, `next_run_at` is advanced and `record_schedule_trigger` persists.
- `schedule_loop` (in `apps/server/src/main.rs`) runs every 60 seconds, checking due schedules via `list_due_schedules`.

## 6. Restart behavior

- `ScanService::recover_active_scan_jobs()` is called during server startup before the job scheduler. It lists all `queued`/`running` scan rows and idempotently calls `ensure_root_job_enqueued` for each.
- Recovery is safe if the root job already exists, already succeeded, or the scan already transitioned terminally.
- If a scan row exists in queued/running state but its root job is missing from `jobs`, the root job is re-enqueued with the same stable `root_job_id`.
- `ensure_root_job_enqueued` is also called on idempotent re-request of an active scan.
- Structured counts and errors are logged; absolute paths are never exposed.
- The durable job engine (Task 4) handles expired leases, retry, and idempotent enqueue.
- Root retry counters are idempotent: `set_scan_counters_absolute` replaces (does not add) discovery counters on retry.

## 7. Diagnostics

- All file diagnostics are stored in `scan_file_diagnostics` with bounded codes and no absolute paths.
- `ScanFileDiagnostic.safe_path_identity` is a 32-char BLAKE3 prefix of the root-relative path.
- Diagnostic persistence failures emit `tracing::warn!` rather than silently discarding.
- Paginated listing via `ScanService::list_scan_diagnostics`.

## 8. Tool configuration

`ScanToolConfig` carries `ffprobe_program: PathBuf` and `exiftool_program: Option<PathBuf>`. These are passed from the server process configuration; they are never serialized into job payloads.

## 9. Automated test coverage

Tests in `core/rcdvlog-server/tests/scan_integration.rs` (26 total):

| Test | Coverage |
| --- | --- |
| `manual_scan_produces_root_job_and_is_claimable` | Root job enqueue + idempotent re-request |
| `full_scan_pipeline_imports_supported_media_and_creates_observations` | End-to-end scan → observation → media objects |
| `spaces_and_non_ascii_filenames_index_correctly` | UTF-8/NFC path handling |
| `unsupported_files_and_symlinks_are_skipped` | Extension filtering + symlink rejection |
| `unchanged_rescan_avoids_redundant_work` | Identity-preserving rescan |
| `full_hash_converges_two_locations_to_one_media_object` | Convergence via full hash |
| `scan_finalizer_waits_for_children_to_settle` | Settlement propagation |
| `scan_diagnostics_are_paginated` | Diagnostic cursor pagination |
| `payload_version_validation_rejects_unknown_version` | Version contract |
| `unknown_job_type_is_terminal_error` | Type contract |
| `modified_ns_round_trip_is_consistent` | Nanosecond precision |
| `scan_list_is_deterministically_paginated` | Scan list pagination |
| `schedule_occurrence_survives_reopen` | Schedule persistence |
| `one_bad_path_does_not_abort_other_files` | Per-file failure isolation |
| `one_bad_file_produces_diagnostic_while_good_file_succeeds` | Strengthened: asserts scan Succeeded, good file media, bad file diagnostic |
| `exiftool_unavailable_stores_ffprobe_data_with_warning` | ExifTool failure stores ffprobe with enrichment_status |
| `file_changed_during_full_hash_commits_nothing` | Full hash fact committed only for stable files |
| `identical_files_converge_through_full_hash` | Two identical files converge to 1 verified media |
| `missing_file_on_rescan_does_not_delete_storage_rows` | Disappearing file preserves storage/media rows |
| `due_schedule_produces_one_scan_and_recovery_idempotent` | Schedule due + recovery idempotency |
| `pagination_scale_with_2000_synthetic_rows` | Deterministic pagination with 2,000 rows |
| `symlink_scope_escape_is_prevented` | Symlink pointing outside binding is contained |
| `recovery_recreates_missing_root_job` | Startup recovery creates missing root job |
| `recovery_does_not_duplicate_existing_root_job` | Recovery is safe when root job exists |
| `all_child_jobs_fail_for_one_file_scan_still_succeeds` | Fully-failed file counters show completed >= failed |
| `migration_4_preserves_job_engine_tables` | Migration 3→4 preserves all job engine tables |

Plus all existing Task 1–4 tests (total 151 tests, all passing).

## 10. Manual checks not run

- Ubuntu/Docker deployment, NFS/SMB mounts, LAN/Tailscale HTTPS
- Sleep/wake, NAS disconnect/reconnect during scan
- Scale with thousands of clips from a real NAS tree
- Resolve import, browser UI, project synchronization
- Memory/file-handle profiling during large scans
