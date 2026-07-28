# S01-T004: Durable Server Job Engine

<!-- bounded-task-metadata
{
  "schema_version": 1,
  "task_id": "S01-T004",
  "epic_id": "S01",
  "title": "Durable Server Job Engine",
  "slug": "durable-server-job-engine",
  "status": "complete",
  "task_class": "foundation",
  "depends_on": [
    "S01-T002"
  ],
  "principal_invariant": "Every job enqueued, claimed, completed, failed, retried, or cancelled follows its state machine exactly, and a server restart preserves every completed result and requeues every expired lease — never silently dropping work or duplicating successful results.",
  "central_falsification": "Enqueue a job, claim it, kill the server process before completion, restart, and assert the job is requeued and claimable again. Then enqueue a job with a specific idempotency key, complete it, restart, re-enqueue with the same key, and assert the original successful result is returned without re-execution.",
  "architecture_decisions_frozen": true,
  "load_bearing_seams": [
    "Job scheduler → durable control catalog",
    "Job handlers → cancellation token contract"
  ],
  "documents_by_role": {
    "contract_editor": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/JOB_ENGINE.md"
    ],
    "implementer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/JOB_ENGINE.md"
    ],
    "independent_tester": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/JOB_ENGINE.md"
    ],
    "reviewer": [
      "docs/tasks/server/ARCHITECTURE.md",
      "docs/tasks/server/JOB_ENGINE.md"
    ],
    "integrator": [
      "docs/tasks/server/ARCHITECTURE.md"
    ]
  },
  "path_ownership": {
    "contract_editor": [
      "docs/tasks/server/tasks/S01-T004-durable-server-job-engine.md"
    ],
    "implementer": [
      "core/rcdvlog-server/src/**",
      "core/batch-estimator/**"
    ],
    "independent_tester": [
      "core/rcdvlog-server/tests/**"
    ],
    "integrator": [
      "Cargo.toml",
      "Cargo.lock",
      "apps/server/src/**"
    ]
  },
  "forbidden_paths": [
    "apps/desktop/**",
    "apps/web/**",
    "apps/cli/**",
    "core/rcdvlog-sync/**",
    "core/rcdvlog-core/src/catalog/**"
  ],
  "verification_commands": [
    "cargo test -p rcdvlog-server",
    "cargo test -p batch-estimator",
    "cargo fmt --all -- --check",
    "cargo clippy --workspace --all-targets -- -D warnings"
  ],
  "reconciliation_checks": [
    {
      "path": "docs/tasks/server/EPIC.md",
      "regex": "\\| 40 \\| \\[S01-T004\\].*\\| complete \\|"
    }
  ]
}
-->

## Goal

Build a restart-safe, SQLite-backed server job engine for scans, hashing,
metadata extraction, thumbnails, proxies, and later server work.

## Principal invariant

This task is correct when every job enqueued, claimed, completed, failed,
retried, or cancelled follows its state machine exactly, and a server restart
preserves every completed result and requeues every expired lease — never
silently dropping work or duplicating successful results.

## Inputs and outputs

Inputs:

- Task 2 server control catalog and migration framework
- `batch-estimator` crate for weighted progress and ETA

Outputs:

- Durable job, attempt, dependency, lease, cancellation, schedule, progress, and rate-history tables
- Job state machine with transactional claim/heartbeat/complete/fail/cancel operations
- Idempotency keys and reusable successful results
- Bounded retry with categorized retryable/terminal failures
- Worker-pool configuration by job class
- `batch-estimator` integration for weighted progress and ETA
- Startup recovery for expired leases
- Generic structured child-process cancellation contract
- Event notifications for later WebSocket/API consumers

## Frozen decisions

1. Claiming a job is an atomic compare-and-transition transaction.
2. Only the current lease holder may heartbeat or complete an attempt.
3. Expired running jobs become eligible for retry on startup or periodic recovery.
4. Completion and output registration are transactionally coordinated by the handler's commit boundary.
5. Re-enqueuing an identical idempotency key returns the existing active or successful job.
6. Cancellation is durable; a restarted worker cannot resurrect canceled work.
7. Failed observations never update throughput history.
8. Rate history keys include node and versioned task recipe.
9. Job payloads are size-bounded and versioned; arbitrary executable commands are forbidden.
10. Error details preserve per-item failures without exposing secrets or unrestricted filesystem paths.
11. Default concurrency: one proxy worker, one full-hash worker, at most two lightweight workers.
12. Scheduler loops use notifications or bounded polling and shut down cleanly.
13. Job events are replayable from durable state; in-memory broadcast is an optimization only.

## Owned paths

- **Implementer:** `core/rcdvlog-server/` job domain, repository, scheduler, worker traits; `core/batch-estimator/` if reusable extension required
- **Independent tester:** `core/rcdvlog-server/tests/` with deterministic fake clocks and handlers
- **Integrator:** workspace `Cargo.toml`, `Cargo.lock`, `apps/server/` startup/shutdown wiring

## Required interface

- Handler trait for job execution consumed by Tasks 5, 6, 16
- Job and event DTOs for downstream consumers
- Scheduler start/stop hooks for server binary wiring

## Acceptance oracle

1. Enqueue, claim, heartbeat, complete, fail, retry, and cancel transitions enforce the state machine.
2. Two workers cannot successfully claim the same attempt.
3. A server restart requeues an expired lease and preserves a non-expired or completed job correctly.
4. Duplicate enqueue requests with one idempotency key do not duplicate work.
5. A terminal failure does not retry; a retryable failure follows bounded backoff.
6. Cancellation survives restart and reaches a running handler through a cancellation token.
7. Successful weighted observations update rate history; failures do not.
8. Progress cannot move backward within one attempt; completed items counted correctly when some fail.
9. Worker concurrency respects configured per-class limits.
10. The queue remains responsive with at least 10,000 synthetic jobs.
11. Shutdown stops new claims and exits without abandoning an unrecorded successful result.

## Independent test charter

Try to falsify: "the job engine never loses or duplicates work across restarts."
Use deterministic fake clocks and fake handlers. Enqueue jobs with varying
retry policies. Claim some, complete some, leave some running. Kill the process.
Restart and verify every job is in the correct state: completed jobs stay
completed, expired leases are requeued, cancelled jobs stay cancelled. Test
concurrent claims with independent SQLite connections. Test idempotency by
re-enqueuing after completion. Test that 10,000 synthetic jobs remain responsive.

## Review charter

Risk domain: job durability and correctness. Questions: (1) Can a job transition
violate the state machine? (2) Can two workers claim the same job? (3) Does a
restart lose completed work? (4) Does a restart resurrect cancelled work? (5)
Does an idempotent re-enqueue duplicate execution? (6) Does shutdown abandon
an unrecorded successful result? Severity: P0 for lost completed work,
duplicated execution, or resurrected cancelled work; P1 for incorrect progress
or rate history. Approval vocabulary: `APPROVE`, `APPROVE_WITH_FOLLOW_UP`,
`REJECT`.

## Non-goals

- Actual scan, fingerprint, FFmpeg, or metadata handlers
- Mac-local durable queue
- Distributed competitive worker claiming
- Web job administration
- Remote job routing

## Stop conditions

Stop and escalate when:

- A handler needs to embed media-specific logic in the scheduler
- A job payload exceeds the size bound
- The scheduler needs filesystem access beyond the control catalog
- Concurrency limits need per-node rather than per-process enforcement

## Commands

```sh
cargo test -p rcdvlog-server
cargo test -p batch-estimator
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

## Handoff

Publish a small handler interface and job/event DTOs for Tasks 5, 6, and 16.
Media-specific logic must not be embedded in the scheduler.
