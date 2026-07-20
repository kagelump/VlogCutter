# Task 5 Detailed Design: NAS Scan and Verified Content Identity

**Parent task:** [TASK_5_nas_scan_and_verified_content_identity.md](TASK_5_nas_scan_and_verified_content_identity.md)  
**Wave:** 1 of 3  
**Implementation owner:** fresh Pi worker session using `deepseek/deepseek-v4-pro`  
**Review:** fresh Pi junior sanity pass after implementation; senior review at the end of Wave 1

## 1. Objective

Implement a restart-safe server scan pipeline that indexes supported media from a confirmed logical storage-root binding, records incremental observation generations, probes technical metadata, computes the existing sampled BLAKE3 fingerprint, verifies full-file BLAKE3 identity, reconciles duplicates and mismatches into the global media library, and persists disabled-by-default schedules. Originals must remain byte-for-byte and metadata-for-metadata untouched.

This task ends with domain services and the server worker runtime needed by later authenticated APIs, the web UI, and CLI. It must not expose an unauthenticated product API on LAN/Tailscale and must not implement representations or project membership.

## 2. Required reading before editing

Read these files completely or the task-relevant sections before making changes:

1. `AGENTS.md`
2. `docs/tasks/server/HANDOFF_AFTER_TASK_4.md`
3. `docs/tasks/server/ARCHITECTURE.md`, especially identity, storage-root, job, and deployment sections
4. `docs/tasks/server/TASK_5_nas_scan_and_verified_content_identity.md`
5. `docs/tasks/server/SCHEMA.md`
6. `docs/tasks/server/STORAGE_LIBRARY.md`
7. `docs/tasks/server/JOB_ENGINE.md`
8. `core/rcdvlog-server/src/catalog.rs`
9. `core/rcdvlog-server/src/library.rs`
10. `core/rcdvlog-server/src/jobs.rs`
11. `core/rcdvlog-server/src/storage_path.rs`
12. media ingest/probe/fingerprint helpers in `core/rcdvlog-core/src/lib.rs`
13. `apps/server/src/main.rs` and `apps/server/src/lib.rs`

Do not modify or rename control-catalog migrations 1–3 or project-catalog migrations 1–4.

## 3. Fixed architectural decisions

These are already accepted; do not reopen them:

- A scan target is `storageRootId + optional RootRelativePath scope`, never an arbitrary absolute path.
- The target resolves only through the current server node's enabled, confirmed local binding.
- Directory walking does not follow symlinks. Every file job re-resolves and canonicalizes its logical path immediately before reading so a post-discovery path change cannot escape containment.
- Supported extensions remain exactly the desktop ingest set unless a deliberately versioned shared helper changes them: `mp4`, `mov`, `m4v`, `mts`, `m2ts`, and `avi`, case-insensitive.
- Sampled BLAKE3 plus size is provisional evidence only. Full-file BLAKE3 plus size is required for verified cross-location convergence.
- A scan generation publishes missing/unavailable conclusions only when the whole requested discovery scope completes successfully. Cancellation, crash, or fatal traversal failure abandons the generation.
- Per-file failures are durable diagnostics and do not abort unrelated files.
- ffprobe is authoritative for technical metadata. ExifTool is optional enrichment; its absence or failure is recorded as an enrichment warning, not a probe failure.
- Original roots are read-only. No temp file, marker, lock, sidecar, log, derived output, or SQLite file may be created beside originals.
- Jobs are Task 4 durable jobs. Handler results are registered through `JobHandler::commit_result` in the same transaction that marks the job succeeded.
- No project membership is created by scanning.
- No unauthenticated product HTTP route is introduced before Task 9. Manual scanning is a Rust domain command in this task; authenticated adapters come later.

## 4. Scope and allowed files

Expected implementation areas:

- `core/rcdvlog-core/src/lib.rs` and focused new modules if needed for shared media primitives
- `core/rcdvlog-server/src/catalog.rs`
- `core/rcdvlog-server/src/library.rs`
- new focused server-core modules such as `scan.rs` and `media_facts.rs`
- `core/rcdvlog-server/src/lib.rs`
- `core/rcdvlog-server/Cargo.toml` and workspace dependencies only when needed
- `apps/server/src/main.rs` / `apps/server/src/lib.rs` for scheduler lifecycle integration, not product endpoints
- server-core, adapter, and process tests
- focused Task 5 documentation and the post-task handoff

Avoid broad formatting or unrelated refactors. Do not edit desktop UI, CLI product commands, web code, deployment TLS/auth, representation generation, or synchronization.

## 5. Forward migration 4

Add one append-only control-catalog migration:

- ID: `4`
- Name: `nas_scan_verified_content_identity`
- Source identity: `server-control/0004-nas-scan-verified-content-identity/20260720`

Increment `CONTROL_CATALOG_SCHEMA_VERSION` to 4. Do not edit the SQL or identities of migrations 1–3.

The migration should add normalized durable facts. Exact internal column names may follow existing style, but preserve the following logical model and constraints.

### 5.1 Scan requests/runs

Persist a scan run keyed by a stable scan ID, with:

- server node ID
- storage root ID
- nullable scope-relative path
- linked observation generation ID
- source (`manual` or `schedule`)
- nullable schedule ID
- lifecycle state (`queued`, `running`, `succeeded`, `failed`, `canceled`)
- discovery counts: visited entries, supported files, unchanged files, queued files, completed files, failed files
- bounded safe last-error code/details (no absolute path)
- created/started/completed timestamps

Enforce one active scan per server node + root + exact scope. A second request for the same active scope must return/reuse the current scan rather than overlap. Wider/narrower scopes may be serialized conservatively if overlap detection is simpler and safer.

### 5.2 Schedules

Persist disabled-by-default schedules with a versioned cadence contract. For this task, use a deliberately small daily schedule shape rather than adding a cron parser:

- stable schedule ID
- storage root ID and nullable scope-relative path
- `enabled`
- local wall-clock time as validated `HH:MM`
- IANA timezone name string (validate non-empty and bounded; resolving next-run times may use `chrono-tz`)
- next run timestamp in UTC
- last enqueued timestamp and nullable last scan ID
- created/updated timestamps

The service must validate that the timezone exists and calculate the next UTC instant deterministically. Disabled schedules do not enqueue. Restart must reload due schedules from SQLite. A uniqueness rule should prevent duplicate enabled schedules for the same root/scope/time/timezone.

### 5.3 Immutable file facts and diagnostics

Persist evidence tied to a storage object and exact file version. The evidence key must include at least size and modification evidence; on Unix include a stable high-resolution modification representation and, if readily available, inode/device evidence for diagnostics. Store:

- quick fingerprint fact with recipe/version and captured timestamp
- full content hash fact with recipe/version, pre-read evidence, post-read evidence, byte count, and captured timestamp
- technical probe fact with recipe/version, ffprobe-derived normalized fields, raw timestamp evidence, rotation, and tool provenance
- optional ExifTool camera make/model plus an enrichment status/warning
- bounded per-file diagnostic records with stable code, stage, storage object ID, safe relative path or hashed path identity, retryability, and timestamp
- mismatch evidence linking the involved storage object/media candidates without exposing an absolute path

Prefer one facts table per stable concept or a well-constrained versioned fact table. Avoid an unrestricted key/value dump. JSON values must be bounded and schema-versioned.

Add indexes for:

- storage object + current evidence lookup
- quick fingerprint + size candidate lookup
- full content hash + size verified lookup
- scan-run and per-file diagnostic lookup
- due schedules

### 5.4 Upgrade and inspection tests

Add a migration test that opens a version-3 control catalog fixture, upgrades to version 4, and verifies existing root/library/job rows survive. Keep read-only schema inspection non-mutating and preserve rejection of newer schemas.

## 6. Shared media primitives

Extract/reuse behavior from `rcdvlog-core`; do not fork a second fingerprint or probe recipe in server-core.

Provide a small public shared API with tests for:

1. `is_supported_media_path(path)` or equivalent, backed by the same extension list used by desktop ingest.
2. `sampled_media_fingerprint(path, known_size)` using the existing sampled BLAKE3 recipe. Preserve the existing recipe exactly; do not silently change which portions are read in this task.
3. a normalized technical probe DTO containing duration, frame rate, encoded dimensions, codec, camera string/make/model when present, raw capture time, normalized UTC best time, metadata rotation, and default rotation.
4. probe entry points that accept configured ffprobe and ExifTool executables as structured `Command` program values. Do not serialize executable paths into jobs.
5. full-file BLAKE3 streaming in bounded memory, with cancellation checks between reads and file evidence captured before and after.

The full hash primitive must return `input_changed` when size or modification evidence changes between the pre-read and post-read stat. It must not return a hash that callers can commit in that case. Read-only open is sufficient; do not request write access.

Keep tool arguments structured and preserve filenames with spaces and non-ASCII characters. Bound stderr included in diagnostics and remove absolute source paths from persisted/logged details.

## 7. Server scan domain API

Add a public `ScanService` (or equivalently named focused service) constructed from the control-catalog path/node identity, `StorageLibrary`, `JobEngine`, and configured tool program names. Expose typed, serde-ready DTOs where later adapters will need them.

Minimum commands/queries:

- request a manual scan for a root and optional relative scope
- get one scan run
- list scan runs with cursor pagination and bounded limits
- create/update/enable/disable/list schedules
- enqueue due schedules idempotently
- list per-file failures/diagnostics for a scan with cursor pagination
- query verification/fact summary for a media or storage object

Every mutation validates IDs and paths before opening the filesystem. Query DTOs must not include node-local absolute mount paths.

Use stable generated identifiers with existing ID conventions (`scan_...`, `generation_...`, `schedule_...`, and the existing typed IDs for jobs/media/storage objects). New public IDs may be validated opaque strings if adding new core identifier wrappers would be disproportionate, but must be bounded and stable.

## 8. Durable job graph and recipes

Use four versioned job types/recipe keys. Keep job payloads typed, bounded, and path-free:

1. `scan.root.v1` / `scan.root.discovery-v1` — lightweight class
2. `scan.quick_fingerprint.v1` / `scan.quick-fingerprint.sampled-v1` — lightweight class
3. `scan.media_probe.v1` / `scan.media-probe.ffprobe-v1` — lightweight class
4. `scan.full_hash.v1` / `scan.full-hash.blake3-v1` — full-hash class

Payloads identify only scan ID, generation ID, storage root ID, storage object ID, and root-relative path as needed. Immutable identity includes evidence/recipe version so an unchanged successful output can be reused safely. Never include absolute paths, tool command strings, shell text, credentials, or arbitrary user JSON.

Implement one dispatching handler registered with the Task 4 scheduler. Reject unknown job types and unsupported payload versions terminally with stable codes.

### 8.1 Root scan handler

The root handler must:

1. Load the scan record and verify it is still eligible.
2. Load the current server's confirmed binding through `StorageLibrary`.
3. Resolve the requested logical scope read-only.
4. Start or resume the linked observation generation. If a prior attempt died after creating it, reuse the same in-progress generation; do not create competing generations for one scan.
5. Walk with `WalkDir::follow_links(false)`.
6. Skip symlinks explicitly, skip unsupported extensions, and turn non-fatal entry errors into per-file/scope diagnostics.
7. Convert each supported entry back to a canonical NFC `RootRelativePath` within the binding/root scope. Reject paths that cannot be safely represented; record and continue.
8. Upsert/find the storage object by `(storageRootId, relativePath)` through a transaction-safe library operation. Do not derive identity from an absolute path.
9. Compare persisted size/modification evidence and successful versioned facts. If unchanged, record the asset observation in this generation and skip redundant work.
10. For new/changed files, enqueue quick-fingerprint and probe jobs. Enqueue full hash after quick identity is registered; either use a durable dependency or enqueue it transactionally from quick-fingerprint result registration.
11. Update scan counters incrementally.
12. Complete the observation generation only after discovery is complete and all required per-file bookkeeping/jobs have reached the task-defined settled point. If generation completion is handled by a finalizer job, make that dependency explicit and idempotent.

Avoid a job graph that requires loading all paths into memory. Thousands of files must be discovered/enqueued incrementally. It is acceptable for the root job to perform discovery synchronously while child jobs continue afterward, but do not publish the generation until every supported discovered file has a durable observation outcome.

### 8.2 Quick fingerprint handler

The quick handler must re-resolve the file from its logical identity, stat before reading, compute the shared sampled fingerprint, stat after reading, and treat evidence changes as retryable `input_changed`.

Its transactional result registration must:

- upsert the storage object's current evidence
- find quick-equal/size-equal media candidates
- create a new provisional media object when no appropriate location exists
- link the storage object provisionally to exactly one provisional media object
- record the accessible observation for the scan generation
- enqueue or establish the idempotent full-hash work needed for verification (if enqueue cannot occur inside the supplied transaction cleanly, arrange the dependency graph at discovery time)

Quick equality never changes two independently discovered locations to verified.

### 8.3 Probe handler

The probe handler re-resolves and probes the file using the configured ffprobe executable and optional ExifTool enrichment. A failed ffprobe is a structured per-file failure; ExifTool failure stores ffprobe data plus an enrichment warning. Persist normalized facts transactionally on success.

### 8.4 Full hash handler

The full-hash handler re-resolves, streams the file, checks cancellation, and captures pre/post evidence. Size/mtime change is retryable `input_changed`; no full hash fact or verified link is committed.

On successful transactional registration:

- persist immutable hash evidence and provenance
- find an existing verified media object by `(fullHash, size)`
- if found, relink/converge this storage location to that media object without duplicating project annotations or deleting facts
- otherwise promote the current provisional media object to verified
- mark the location verified only after the global media object is verified
- if the provisional quick candidate resolves to a different full hash, keep distinct media objects/locations and record a `quick_fingerprint_mismatch` diagnostic; never merge them
- make retries/idempotent duplicate delivery produce the same final state

Because `media_object_locations.storage_object_id` is unique, relinking must be a deliberate transactional library operation. Preserve old media objects if they still have another location or project membership. Do not delete historical media or facts merely because a location moved.

## 9. Library reconciliation changes

Add focused `StorageLibrary` methods rather than editing SQLite from handlers ad hoc. Likely operations include:

- get-or-create/upsert storage object by logical root/path
- find current location/media for a storage object
- find candidate media by quick fingerprint + size
- find verified media by full hash + size
- transaction-safe registration of quick identity, probe fact, full identity, mismatch, and observation outcome
- list verification diagnostics/facts

Where Task 4 calls `commit_result` with a `rusqlite::Transaction`, provide `pub(crate)` transaction helpers in the library/scan module so result registration and job success are atomic.

Preserve pagination order and existing public DTO compatibility. Do not add project annotations to the global library.

## 10. Scan completion and failures

Define settled behavior precisely:

- A supported file counts complete after it has an accessible observation plus durable quick identity outcome; probe and full hash may settle independently but their failures must be visible.
- A file that cannot be opened/probed/hashed records a safe diagnostic and a failed item count. Other files continue.
- A fatal root traversal/binding failure marks the scan failed and abandons the generation.
- Cancellation marks the scan canceled and abandons the generation.
- Only a successful scan marks the generation completed, making previously unseen objects unavailable through the existing generation projection.
- Retrying after restart reuses idempotency keys and does not duplicate media, storage objects, observations, or diagnostics.

Persist safe details such as stage, code, retryability, storage IDs, and relative path. Never persist an absolute path or uncontrolled external-tool stderr.

## 11. Scheduled execution and server lifecycle

Add a small scheduler loop that periodically checks due enabled schedules and idempotently requests scans. It must:

- derive idempotency from schedule ID + scheduled UTC occurrence
- update next-run UTC after enqueue
- not overlap the same root/scope
- survive restart because schedule state is in SQLite
- stop through the existing cancellation token

Integrate the Task 4 `JobScheduler` and the schedule loop into `apps/server/src/main.rs` (or an adapter-owned runtime supervisor). Use one shared shutdown token, wait for bounded drain, and checkpoint afterward. A handler/job error is persisted and must not crash the HTTP liveness server.

Do not add public scan HTTP routes. Tests may invoke the domain service directly.

## 12. Deterministic tests

Add focused tests covering every parent-task acceptance criterion.

### 12.1 Pure/unit tests

- extension matching is shared and case-insensitive
- sampled fingerprint exactly matches legacy desktop behavior
- full hash cancellation and pre/post evidence validation
- schedule time/timezone validation and next-run calculation
- payload/version validation and rejection of unknown job types
- safe diagnostic redaction/bounds

### 12.2 SQLite/domain tests

- migration 3 -> 4 preserves existing rows
- manual scan request is idempotent while active
- unchanged rescan reuses successful facts and avoids redundant jobs
- interrupted/abandoned generation does not hide older observations
- later successful rescan publishes unavailable state without deleting storage/media records
- quick-equal/full-equal locations converge
- forced quick collision/full mismatch remains distinct and records warning
- job result plus fact registration is atomic under injected failure
- due schedules survive a service reopen/restart and do not overlap
- pagination remains deterministic with thousands of synthetic metadata rows

### 12.3 Filesystem/media integration tests

Use only `tempfile` roots. Generate a few one-second clips with FFmpeg when available and skip clearly when FFmpeg/ffprobe are absent. Include spaces and non-ASCII names. Verify:

- recursive import of supported extensions
- unsupported entries and symlinks are skipped
- ExifTool failure preserves ffprobe metadata
- one deliberately bad media file records a failure while another succeeds
- file mutation during full hash yields `input_changed` and commits no full hash
- before/after recursive snapshots of the read-only fixture tree show no new/deleted/changed entries, contents, or metadata caused by scanning

Never use `/Volumes`, the configured real NAS fixture, or `~/Movies/VideoProjects` in automated tests.

## 13. Validation commands

Run while iterating:

```bash
cargo fmt --all -- --check
cargo test -p rcdvlog-core
cargo test -p rcdvlog-server-core
cargo test -p rcdvlog-server
cargo clippy -p rcdvlog-server-core -p rcdvlog-server --all-targets -- -D warnings
```

Before reporting completion run:

```bash
pnpm check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

If a media test skips, report the exact missing tool. Do not claim Ubuntu/NAS/Docker manual acceptance.

## 14. Documentation and handoff

Update or add a focused scan contract document describing implemented behavior, schemas, job recipes, safe diagnostics, schedule semantics, and missing manual acceptance. Update `TASK_5_nas_scan_and_verified_content_identity.md` status only when criteria are actually met. Create `HANDOFF_AFTER_TASK_5.md` with:

- exact migrations and immutable identities
- public types/services
- recipe/job keys
- verified commands and test counts
- known manual checks not run
- Task 6 starting constraints

Do not broadly update README/AGENTS/PRD; Task 20 owns the repository-wide audit unless a currently false statement would actively mislead the next task.

## 15. Git and reporting policy

- Do not commit, merge, rebase, or push. The orchestrator owns checkpoints.
- Preserve all pre-existing work.
- Do not create generated media or databases inside tracked source directories.
- Final report must list changed files, summarize schema/API/behavior, give exact validation results, call out skipped/manual checks, and state any remaining risk.
- If an accepted architectural constraint cannot be met, stop and report the blocker instead of inventing a competing architecture.

