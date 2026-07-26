# Task 4: Durable Server Job Engine

**Status:** Planned
**Depends on:** Task 2
**Unblocks:** Tasks 5, 6, 16, and 17

## Objective

Build a restart-safe, SQLite-backed server job engine for scans, hashing, metadata extraction, thumbnails, proxies, and later server work.

## User outcome

Long-running server work can be queued, monitored, canceled, retried, and resumed after a process or machine restart without losing completed results or corrupting inputs.

## Scope

- Add durable job, attempt, dependency, lease, cancellation, schedule, progress, and rate-history tables to the control catalog.
- Implement the job state machine and transactional claim/heartbeat/complete/fail/cancel operations.
- Implement idempotency keys and reusable successful results.
- Implement bounded retry with categorized retryable/terminal failures.
- Implement worker-pool configuration by job class.
- Integrate `batch-estimator` for weighted progress and ETA.
- Add startup recovery for expired leases.
- Add a generic structured child-process cancellation contract for later FFmpeg/ffprobe work.
- Add event notifications consumable by later WebSocket/API tasks.

## Out of scope

- Actual scan, fingerprint, FFmpeg, or metadata handlers
- Mac-local durable queue
- Distributed competitive worker claiming
- Web job administration
- Remote job routing

## Job contract

Jobs include:

- Stable `jobId`
- Versioned `jobType`
- Optional project, media, storage root, and representation IDs
- Target node
- Requirements and immutable input identity
- Recipe/settings payload and schema version
- Idempotency key
- Priority and enqueue sequence
- State, lease owner, lease expiry, and heartbeat
- Attempt count and retry policy
- Weighted progress and aggregate estimate
- Structured error code/details
- Created, started, updated, and completed timestamps
- Optional immutable output identity

## Design requirements

1. Claiming a job is an atomic compare-and-transition transaction.
2. Only the current lease holder may heartbeat or complete an attempt.
3. Expired running jobs become eligible for retry on startup or periodic recovery.
4. Completion and output registration are transactionally coordinated by the handler's commit boundary.
5. Re-enqueuing an identical idempotency key returns the existing active or successful job as appropriate.
6. Cancellation is durable. A restarted worker cannot resurrect canceled work.
7. Failed observations never update throughput history.
8. Rate history keys include node and versioned task recipe.
9. Job payloads are size-bounded and versioned; arbitrary executable commands are forbidden.
10. Error details preserve per-item failures without exposing secrets or unrestricted filesystem paths.
11. Default concurrency is one proxy worker, one full-hash worker, and at most two lightweight workers.
12. Scheduler loops use notifications or bounded polling and shut down cleanly.
13. Job events are replayable from durable state; an in-memory broadcast is only an optimization.

## Likely implementation areas

- `core/rcdvlog-server` job domain, repository, scheduler, and worker traits
- `core/batch-estimator` only if a genuinely reusable extension is required
- `apps/server` startup/shutdown wiring
- Control-catalog migrations and temporary-database tests

## Acceptance criteria

- [ ] Enqueue, claim, heartbeat, complete, fail, retry, and cancel transitions enforce the state machine.
- [ ] Two workers cannot successfully claim the same attempt.
- [ ] A server restart requeues an expired lease and preserves a non-expired or completed job correctly.
- [ ] Duplicate enqueue requests with one idempotency key do not duplicate work.
- [ ] A terminal failure does not retry; a retryable failure follows bounded backoff.
- [ ] Cancellation survives restart and reaches a running handler through a cancellation token.
- [ ] Successful weighted observations update rate history; failures do not.
- [ ] Progress cannot move backward within one attempt and completed items are counted correctly when some fail.
- [ ] Worker concurrency respects configured per-class limits.
- [ ] The queue remains responsive with at least 10,000 synthetic jobs in a temporary catalog.
- [ ] Shutdown stops new claims and exits without abandoning an unrecorded successful result.

## Verification

Use deterministic fake clocks and fake handlers for lease/retry tests. Add concurrency tests with independent SQLite connections. Run the workspace formatting, Clippy, and test gates.

## Handoff notes

Publish a small handler interface and job/event DTOs for Tasks 5, 6, and 16. Media-specific logic must not be embedded in the scheduler.
